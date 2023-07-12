mod deep_lynx;
mod errors;
mod tests;

use crate::deep_lynx::{DeepLynxAPI, InitiateDataSourceDownloadQuery};
use crate::errors::LoaderError;
use chrono::NaiveDateTime;
use duckdb::types::{TimeUnit, Value};
use duckdb::{AccessMode, Config, OptionalExt, Row};
use log::{debug, info};
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use serde_yaml::from_reader;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::{fs, io};
use uuid::Uuid;

#[pyclass]
#[derive(Debug, Clone)]
pub struct Loader {
    config: Configuration,
    client: DeepLynxAPI,
    // you might ask why we don't hold the duckdb connection open - that's because we really don't
    // want to hold a rw connection open while the python code runs, in case they want to use it for
    // something
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    api_key: Option<String>,
    api_secret: Option<String>,
    deeplynx_url: String,
    db_path: String,
    refresh_interval: u64,
    data_retention_days: u32,
    target_data_source_id: Option<u64>,
    target_container_id: Option<u64>,
    debug: Option<bool>,
    data_sources: Vec<DataSourceConfiguration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataSourceConfiguration {
    table_name: String,
    container_id: u64,
    data_source_id: u64,
    timestamp_column_name: String,
    secondary_index: Option<String>,
    initial_timestamp: Option<String>,
    initial_index_start: Option<u64>,
}

/// A Python module implemented in Rust.
#[pymodule]
fn deeplynx_loader(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<Loader>()?;
    Ok(())
}

#[pymethods]
impl Loader {
    #[new]
    pub fn new(config_file_path: &str) -> Result<Self, LoaderError> {
        let config_file = File::open(config_file_path)?;
        let config: Configuration = from_reader(config_file)?;

        let mut log_level = log::LevelFilter::Error;

        if config.debug.is_some() {
            log_level = log::LevelFilter::Debug
        }

        // set the log to output to file and stdout
        fern::Dispatch::new()
            .format(|out, message, record| {
                out.finish(format_args!(
                    "{}[{}][{}] {}",
                    chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                    record.target(),
                    record.level(),
                    message
                ))
            })
            .level(log_level)
            .chain(std::io::stdout())
            .chain(fern::log_file("deeplynx_loader.log")?)
            .apply()?;

        let mut client = DeepLynxAPI::new(
            config.deeplynx_url.clone(),
            config.api_key.clone(),
            config.api_secret.clone(),
        )?;

        Ok(Loader { config, client })
    }

    pub fn load_data(&self) -> Result<(), LoaderError> {
        // open the duckdb connection outside the thread so we can indicate failure if needed
        let db_path = Path::new(self.config.db_path.as_str());
        let conn = duckdb::Connection::open_with_flags(
            db_path,
            Config::default().access_mode(AccessMode::ReadWrite)?,
        )?;

        load_loop(&self.config, &conn)?;

        match conn.close() {
            Ok(_) => Ok(()),
            Err(ce) => Err(LoaderError::DuckDB(ce.1)),
        }
    }

    pub fn send_file(&mut self, file_path: &str, data_source_id: u64) -> Result<(), LoaderError> {
        self.client.import(
            self.config
                .target_container_id
                .ok_or(LoaderError::UnwrapOption)?,
            data_source_id,
            Some(PathBuf::from(file_path)),
            None,
        )?;

        Ok(())
    }
}

fn load_loop(config: &Configuration, conn: &duckdb::Connection) -> Result<(), LoaderError> {
    for data_source in &config.data_sources {
        let mut client = DeepLynxAPI::new(
            config.deeplynx_url.clone(),
            config.api_key.clone(),
            config.api_secret.clone(),
        )?;
        // we could run this check just once on startup instead of checking each time, but this is more robust
        // and we have no idea what kind of SQL the other users might be running on it - changes how
        // we load data in
        let table_exists: Option<String> = conn
            .query_row(
                "SELECT table_name FROM duckdb_tables() WHERE table_name = ?",
                [data_source.table_name.clone()],
                |row| row.get(0),
            )
            .optional()?;

        // if we don't have a table, treat this is as an initial fetch so the table gets created
        if table_exists.is_none() {
            debug!(
                "table {} does not exist, running initial fetch",
                data_source.table_name.clone()
            );
            initial_fetch_and_load(config, data_source, &mut client, conn)?;
            continue;
        }

        debug!(
            "table {} exists, running continual fetch",
            data_source.table_name.clone()
        );
        continuous_fetch_and_load(config, data_source, &mut client, conn)?;

        // run the data clean functionality
        clean_data(config, data_source, conn)?;
    }

    Ok(())
}

pub fn clean_data(
    config: &Configuration,
    data_source: &DataSourceConfiguration,
    conn: &duckdb::Connection,
) -> Result<(), LoaderError> {
    conn.execute(
        format!(
            "DELETE FROM {} WHERE {} < NOW() - interval '{}' days",
            data_source.table_name, data_source.timestamp_column_name, config.data_retention_days
        )
        .as_str(),
        [],
    )?;
    Ok(())
}

// if data or table already exists for a data source, then we fetch continuously
pub fn continuous_fetch_and_load(
    config: &Configuration,
    data_source: &DataSourceConfiguration,
    client: &mut DeepLynxAPI,
    conn: &duckdb::Connection,
) -> Result<(), LoaderError> {
    // we need to fetch the last record in the table, but the sort isn't guaranteed so we'll do that
    // manually
    let mut check_query = format!(
        "SELECT {} FROM {} ORDER BY {} DESC LIMIT 1",
        data_source.timestamp_column_name,
        data_source.table_name,
        data_source.timestamp_column_name
    );

    // sort by secondary index as well if it exists, get the latest value
    if (data_source.secondary_index.is_some()) {
        let secondary_index = data_source
            .secondary_index
            .clone()
            .ok_or(LoaderError::UnwrapOption)?;

        check_query = format!(
            "SELECT {},{} FROM {} ORDER BY {} DESC,{} DESC LIMIT 1",
            data_source.timestamp_column_name,
            secondary_index,
            data_source.table_name,
            data_source.timestamp_column_name,
            secondary_index
        );
    }

    // simple struct representing the record from the DB
    struct Record {
        timestamp_or_index: duckdb::types::Value, // the api expects a string even if it's an index number
        secondary_index: Option<u64>,
    }

    // pull the last record if it exists
    let last_record = conn
        .query_row(check_query.as_str(), [], |row: &Row| {
            let mut secondary_index: Option<u64> = None;
            if data_source.secondary_index.is_some() {
                secondary_index = Some(row.get(1)?);
            }

            Ok(Record {
                timestamp_or_index: row.get(0)?,
                secondary_index,
            })
        })
        .optional()?;

    // if we don't have a last record, we need to drop the table and run initial fetch and load again
    if last_record.is_none() {
        return initial_fetch_and_load(config, data_source, client, conn);
    }

    let last_record = last_record.ok_or(LoaderError::UnwrapOption)?;

    // because we need to handle either an index or timestamp we have to match through duckdb's type
    // and convert to what we need - super fun!
    let start_time: Option<String> = match last_record.timestamp_or_index {
        Value::Null => None,
        Value::Boolean(b) => Some(b.to_string()),
        Value::TinyInt(t) => Some(t.to_string()),
        Value::SmallInt(s) => Some(s.to_string()),
        Value::Int(i) => Some(i.to_string()),
        Value::BigInt(b) => Some(b.to_string()),
        Value::HugeInt(h) => Some(h.to_string()),
        Value::UTinyInt(u) => Some(u.to_string()),
        Value::USmallInt(s) => Some(s.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::UBigInt(b) => Some(b.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Decimal(d) => Some(d.to_string()),
        Value::Timestamp(unit, v) => match unit {
            TimeUnit::Second => Some(
                NaiveDateTime::from_timestamp_opt(v, 0)
                    .ok_or(LoaderError::Database)?
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),
            TimeUnit::Millisecond => Some(
                NaiveDateTime::from_timestamp_millis(v)
                    .ok_or(LoaderError::Database)?
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),

            TimeUnit::Microsecond => Some(
                NaiveDateTime::from_timestamp_micros(v)
                    .ok_or(LoaderError::Database)?
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),
            TimeUnit::Nanosecond => None, // we can't parse out nanos - thankfully they shouldn't come this way
        },
        Value::Text(s) => Some(s),
        Value::Blob(v) => Some(std::str::from_utf8(v.as_slice())?.to_string()),
        Value::Date32(d) => Some(d.to_string()),
        Value::Time64(unit, v) => match unit {
            TimeUnit::Second => Some(
                NaiveDateTime::from_timestamp_opt(v, 0)
                    .ok_or(LoaderError::Database)?
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),
            TimeUnit::Millisecond => Some(
                NaiveDateTime::from_timestamp_millis(v)
                    .ok_or(LoaderError::Database)?
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),

            TimeUnit::Microsecond => Some(
                NaiveDateTime::from_timestamp_micros(v)
                    .ok_or(LoaderError::Database)?
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ),
            TimeUnit::Nanosecond => None, // we can't parse out nanos - thankfully they shouldn't come this way
        },
    };

    // first we fetch the file pointer for the download, this way we can check filesize against disk
    // passing in the elements provided by the user, if none provided will default to returning
    // the full table currently
    let file_pointer = client.initiate_data_source_download(
        data_source.container_id,
        data_source.data_source_id,
        InitiateDataSourceDownloadQuery {
            start_time,
            end_time: None, // deeplynx defaults to latest timestamp if no endpoint is provided
            secondary_index_name: data_source.secondary_index.clone(),
            secondary_index_start_value: match last_record.secondary_index {
                None => Some(0),
                Some(i) => Some(i),
            },
        },
    )?;

    // TODO: check file against disk size prior to downloading

    let mut file_stream =
        client.download_file(data_source.container_id, file_pointer.id.parse()?, true)?;

    // copy the file stream from the download to a temporary file
    let uuid = Uuid::new_v4();
    let mut file = File::create(format!("{uuid}.csv"))?;
    io::copy(&mut file_stream, &mut file)?;

    // create a table from that .csv and load it in duckdb
    let inserted = conn.execute(
        format!(
            "COPY {} FROM '{uuid}.csv' (HEADER TRUE)",
            data_source.table_name
        )
        .as_str(),
        [],
    )?;

    if inserted == 0 {
        debug!(
            "no data inserted for data source {} on continuous fetch, continuing loop",
            data_source.data_source_id
        );
    }

    fs::remove_file(format!("{uuid}.csv"))?;
    Ok(())
}

// the first loading call for a data source, ensures a table is created if data exists - this is a
// DESTRUCTIVE operation as it will first wipe the table if it exists to insure that the table matches
// the latest data from the source
pub fn initial_fetch_and_load(
    config: &Configuration,
    data_source: &DataSourceConfiguration,
    client: &mut DeepLynxAPI,
    conn: &duckdb::Connection,
) -> Result<(), LoaderError> {
    // first drop the table if it exists so that we can guarantee its the right structure
    conn.execute(
        format!("DROP TABLE IF EXISTS {}", data_source.table_name).as_str(),
        [],
    )?;

    // first we fetch the file pointer for the download, this way we can check filesize against disk
    // passing in the elements provided by the user, if none provided will default to returning
    // the full table currently
    let file_pointer = client.initiate_data_source_download(
        data_source.container_id,
        data_source.data_source_id,
        InitiateDataSourceDownloadQuery {
            start_time: data_source.initial_timestamp.clone(),
            end_time: None, // deeplynx defaults to latest timestamp if no endpoint is provided
            secondary_index_name: data_source.secondary_index.clone(),
            secondary_index_start_value: Some(0),
        },
    )?;

    // TODO: check file against disk size prior to downloading

    let mut file_stream =
        client.download_file(data_source.container_id, file_pointer.id.parse()?, true)?;

    // copy the file stream from the download to a temporary file
    let uuid = Uuid::new_v4();
    let mut file = File::create(format!("{uuid}.csv"))?;
    io::copy(&mut file_stream, &mut file)?;

    // create a table from that .csv and load it in duckdb
    let inserted = conn.execute(
        format!(
            "CREATE TABLE {} AS SELECT * FROM '{uuid}.csv'",
            data_source.table_name
        )
        .as_str(),
        [],
    )?;

    // if no rows are inserted we need to remove the table as the inference of the data types might
    // be incorrect, when the process loops again it will attempt to create the table again if it
    // doesn't already exist - don't error out because lack of data doesn't constitute an error state
    // but do log it
    if inserted == 0 {
        debug!(
            "no data fetched for data source {}, dropping temporary table",
            data_source.data_source_id
        );
        conn.execute(
            format!("DROP TABLE IF EXISTS {}", data_source.table_name).as_str(),
            [],
        )?;
    }

    fs::remove_file(format!("{uuid}.csv"))?;
    Ok(())
}
