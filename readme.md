# deeplynx-loader
_A Python module managing timeseries data from DeepLynx locally_
_______
`deeplynx-loader` is a library designed to make it as easy as possible for users to download and access timeseries or tabular data from DeepLynx. This library is available as both a Python module. While we currently only target Python, `deeplynx-loader` can be easily adapted to other project paradigms.


## How it works
______
You can think of `deeplynx-loader` as a type of local cache of timeseries data from DeepLynx. We use a database called [DuckDB](https://duckdb.org/) to store the timeseries data directly on the machine where your code is running. **We handle all the communication with DeepLynx, so you don't have to.**

______

## Building From Source (Python)
### Requirements
* Rust > 1.48
* Python > 3.7
* A Python virtual environment (we recommend `pyenv`)
* `maturin` Python package installed - [site](https://github.com/PyO3/maturin)

Once you have the requirements met, building and installing from source is fairly trivial.

1.  Create and activate a python 3.7 or above virtual environment. See the documentation for whichever tool you choose to handle this. This [StackOverflow Question](https://stackoverflow.com/questions/62841295/how-can-i-activate-a-virtual-env-with-pyenv) explains how to do it pretty well.
2.  You have two options for now building and installing the python package - **Note**: you must rerun either of these commands each time you make a change to the underlying Rust code.
   1. (recommended) - navigate to the directory in which you cloned this repository and run `maturin develop` in your console/terminal. This will build the python module directly into the virtual environment you've activated.
   2. navigate to the directory in which you cloned the repository and run `maturin build --release`. Then you must navigate to the target folder and find the output of the build under `wheels`. You will then install that wheel manually by doing `pip install {name}.whl`

## Installing From Releases
1.  Included in this repository you should see various releases. Choose the latest release and download the zip/tar file who's name represents the operating system your code will be running on.
2.  Unzip/tar the downloaded release or if the file is already a `.whl` file, skip to the next step.
3.  Run  `pip install {name of the file}.whl` - the module should successfully install into your current python environment
________

## Usage and Configuration
Prior to using the `deeplynx-loader` module, you must create a configuration YAML file. The module needs this file to know how to talk to DeepLynx, what data sources to fetch, and various other pieces of information. If you need help generating this file, talk to your local DeepLynx administrator. A config sample is included below with explanations of each field and how to find them.
### Config Sample
```yaml
api_key: "generated from DeepLynx's GUI, found under access management"
api_secret: "generated from DeepLynx's GUI, found under access management"
deeplynx_url: "the deeplynx instance your data resides on, no trailing slash"
data_retention_days: 30 # how long the data should be allowed to stay in the db, only applicable if your primary_timestamp column is indeed a timestamp
refresh_interval: 5 # how often to check DeepLynx for new data, in seconds
debug: true # logging level, set to anything to enable debugging , remove to stop
db_path: "./test.db" # where the database you want to access should be saved - should end with the `db` or `duckdb` extension
target_data_source_id: "when you call the 'send' method, the data source where the data should go"
target_container_id: "when you call the 'send' method,the container where the data should go"
data_sources: # a list of all the data sources to fetch - each one of these will generate a table in the duckdb
  - data_source_id: 1 # id of the target data source
    container_id: 1 # id of the container that the data source lives in
    name: "table name must be lowercase, contain no spaces, and only have _ as special characters" 
    timestamp_column_name: "name of timestamp or primary index name, found on the edit timeseries data source screen"
    secondary_index: "(optional) secondary column index name , helpful when rows share same timestamp but are indexed, initial value is 0"
    initial_timestamp: "(optional) timestamp, if not included will default to 1 day"
    initial_index_start:  0 #  (optional) initial index value to start search on will default to timestamp if this value is not provided
```

Once you have your configuration yaml file saved, using the module in your code is as easy as the sample below.
## Python Code Sample
```python
import deeplynx_loader
import time

# only needs the path where the config yaml is stored
# we recommend wrapping all calls to the loader in a try/catch block to capture any panics
try:
    loader = deeplynx_loader.Loader(".config.yml")
except Exception as excep:
    print(excep)

# as long as your code runs, deeplynx-loader will run in the background fetching new data
while True:
    try:
        loader = loader.load_data() # fetchs latest data from deeplynx and stores it in the duckdb
    except Exception as excep:
        print(excep)
        
    print("Python Code Loop")


# upload a csv file to the target data source and container id stored in the config.yml
try:
    loader.send_file("csv file")
except Exception as excep:
    print(excep)

```

### Data Retrieval
Now that `deeplynx-loader` has created and is continually loading data into the DuckDB database, you need to be able to get that data out. The below sample uses the Python DuckDB SDK in order to run a simple query and convert that to a data frame. You can find more info [here](https://duckdb.org/docs/api/python/overview).

```python
import duckdb
import pandas as pd

# create a connection to a file called named the same as in your configuration value, same path 
con = duckdb.connect('file.db')

# run a simple query selecting all data from your table and instantly converting it into a pandas data frame
duckdb.sql('SELECT * FROM table_name').df()
```