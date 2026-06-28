use duckdb::Connection;

/// Open a fresh in-memory DuckDB connection for a single vector load.
pub(crate) fn open_in_memory() -> Result<Connection, String> {
    Connection::open_in_memory().map_err(|error| format!("Could not open DuckDB: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_memory_and_runs_a_query() {
        let conn = open_in_memory().expect("open in-memory DuckDB");
        let value: i64 = conn
            .query_row("SELECT 1", [], |row| row.get(0))
            .expect("select 1");
        assert_eq!(value, 1);
    }
}
