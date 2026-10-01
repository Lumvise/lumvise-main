use crate::Result;
use crate::local::sql::schema::initialize_sql_schema;
use rusqlite::Connection;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const LUMVISE_DB_READERS_ENV: &str = "LUMVISE_DB_READERS";
const DEFAULT_PERSISTENT_READER_COUNT: usize = 8;
const MIN_PERSISTENT_READER_COUNT: usize = 1;

pub(crate) struct SqlConnections {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
}

impl SqlConnections {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let writer = open_writer_connection(path)?;
        initialize_sql_schema(&writer)?;
        let readers = open_reader_connections(path)?;
        Ok(Self::new(writer, readers))
    }

    pub(crate) fn in_memory() -> Result<Self> {
        let writer = Connection::open_in_memory()?;
        configure_writer_connection(&writer)?;
        initialize_sql_schema(&writer)?;
        Ok(Self::new(writer, Vec::new()))
    }

    pub(crate) fn read_conn(&self) -> MutexGuard<'_, Connection> {
        if self.readers.is_empty() {
            return self.write_conn();
        }
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.readers[index]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn write_conn(&self) -> MutexGuard<'_, Connection> {
        let wait_started = Instant::now();
        let guard = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        metrics::histogram!("lumvise_db_writer_gate_wait_seconds", "gate" => "sql")
            .record(wait_started.elapsed().as_secs_f64());
        guard
    }

    fn new(writer: Connection, readers: Vec<Connection>) -> Self {
        Self {
            writer: Mutex::new(writer),
            readers: readers.into_iter().map(Mutex::new).collect(),
            next_reader: AtomicUsize::new(0),
        }
    }
}

fn open_writer_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure_writer_connection(&conn)?;
    Ok(conn)
}

fn open_reader_connections(path: &Path) -> Result<Vec<Connection>> {
    let count = read_persistent_reader_count();
    let mut readers = Vec::with_capacity(count);
    for _ in 0..count {
        let conn = Connection::open(path)?;
        configure_reader_connection(&conn)?;
        readers.push(conn);
    }
    Ok(readers)
}

fn configure_writer_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;",
    )?;
    Ok(())
}

fn configure_reader_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA query_only = ON;",
    )?;
    Ok(())
}

fn read_persistent_reader_count() -> usize {
    parse_persistent_reader_count(std::env::var(LUMVISE_DB_READERS_ENV).ok().as_deref())
}

fn parse_persistent_reader_count(raw: Option<&str>) -> usize {
    let count = match raw {
        Some(raw_count) => raw_count
            .parse::<usize>()
            .ok()
            .filter(|value| *value >= MIN_PERSISTENT_READER_COUNT),
        None => None,
    };
    count.unwrap_or(DEFAULT_PERSISTENT_READER_COUNT)
}
