#[cfg(test)]
mod deeplynx_loader_tests {
    use crate::deep_lynx::DeepLynxAPI;
    use crate::errors::LoaderError;
    use crate::{continuous_fetch_and_load, initial_fetch_and_load, Configuration};
    use duckdb::{AccessMode, Config, OptionalExt};
    use serde_yaml::from_reader;
    use std::fs;
    use std::fs::File;

    #[test]
    fn initial_process_functionality() -> Result<(), LoaderError> {
        // double check we're starting from scratch - ignore possible not existing error
        let _ = fs::remove_file("./test.db");
        let config_file = File::open("./.config.yml")?;
        let config: Configuration = from_reader(config_file)?;

        let conn = duckdb::Connection::open_with_flags(
            config.db_path.clone(),
            Config::default().access_mode(AccessMode::ReadWrite)?,
        )?;

        let mut api = DeepLynxAPI::new(
            config.deeplynx_url.clone(),
            config.api_key.clone(),
            config.api_secret.clone(),
        )?;

        initial_fetch_and_load(&config, &config.data_sources[0], &mut api, &conn)?;

        // checking if the table now exists should be enough
        let table_exists: Option<String> = conn
            .query_row(
                "SELECT table_name FROM duckdb_tables() WHERE table_name = ?",
                [config.data_sources[0].table_name.clone()],
                |row| row.get(0),
            )
            .optional()?;

        assert!(table_exists.is_some());
        fs::remove_file("./test.db")?;
        Ok(())
    }

    #[test]
    fn continuous_process_functionality() -> Result<(), LoaderError> {
        // double check we're starting from scratch - ignore possible not existing error
        let _ = fs::remove_file("./test.db");
        let config_file = File::open("./.config.yml")?;
        let config: Configuration = from_reader(config_file)?;

        let conn = duckdb::Connection::open_with_flags(
            config.db_path.clone(),
            Config::default().access_mode(AccessMode::ReadWrite)?,
        )?;

        let mut api = DeepLynxAPI::new(
            config.deeplynx_url.clone(),
            config.api_key.clone(),
            config.api_secret.clone(),
        )?;

        initial_fetch_and_load(&config, &config.data_sources[0], &mut api, &conn)?;

        // checking if the table now exists should be enough
        let table_exists: Option<String> = conn
            .query_row(
                "SELECT table_name FROM duckdb_tables() WHERE table_name = ?",
                [config.data_sources[0].table_name.clone()],
                |row| row.get(0),
            )
            .optional()?;

        assert!(table_exists.is_some());

        // now lets run the continuous test - because we can't load DL with more data this test
        // should not be completely trusted, but we're getting as close as we can without mocking
        // a crap ton of data
        continuous_fetch_and_load(&config, &config.data_sources[0], &mut api, &conn)?;

        fs::remove_file("./test.db")?;
        Ok(())
    }
}
