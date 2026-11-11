use super::*;

#[test]
fn owning_connection_cache_bounds_eviction_clears_bindings_and_flushes_schema() -> TestResult {
    let directory = TestDirectory::new()?;
    let database = seed(&directory, &["a".into()])?;
    let connection = &database.connection;

    for value in 0..=STATEMENT_CACHE_CAPACITY {
        // Finite test statements, never caller-controlled SQL text.
        let sql = format!("SELECT {value}");
        let mut statement = connection.prepare_cached(&sql)?;
        assert_eq!(statement.get_status(StatementStatus::Run), 0);

        assert_eq!(
            statement.query_row([], |row| row.get::<_, i64>(0))?,
            i64::try_from(value)?
        );
    }

    // Seventeen distinct statements retain exactly the latest sixteen.
    for value in [1, STATEMENT_CACHE_CAPACITY] {
        let sql = format!("SELECT {value}");
        let statement = connection.prepare_cached(&sql)?;
        assert_eq!(statement.get_status(StatementStatus::Run), 1);
    }

    {
        let statement = connection.prepare_cached("SELECT 0")?;
        assert_eq!(statement.get_status(StatementStatus::Run), 0);
    }

    {
        let mut statement = connection.prepare_cached("SELECT ?1")?;
        let value: Vec<u8> = statement.query_row([b"canary"], |row| row.get(0))?;
        assert_eq!(value, b"canary");
    }

    {
        let mut statement = connection.prepare_cached("SELECT ?1")?;
        assert_eq!(statement.get_status(StatementStatus::Run), 1);
        let mut rows = statement.raw_query();
        assert_eq!(rows.next()?.unwrap().get::<_, Option<Vec<u8>>>(0)?, None);
    }

    let sql = "SELECT name FROM buckets WHERE bucket_id = ?1";

    {
        let mut statement = connection.prepare_cached(sql)?;

        assert_eq!(
            statement.query_row([BUCKET], |row| row.get::<_, String>(0))?,
            "photos"
        );
    }

    connection.execute(
        "UPDATE buckets SET name = 'images' WHERE bucket_id = ?1",
        [BUCKET],
    )?;

    {
        let mut statement = connection.prepare_cached(sql)?;
        assert_eq!(statement.get_status(StatementStatus::Run), 1);

        assert_eq!(
            statement.query_row([BUCKET], |row| row.get::<_, String>(0))?,
            "images"
        );
    }

    connection.flush_prepared_statement_cache();

    connection.execute_batch(
        "BEGIN IMMEDIATE; ALTER TABLE buckets ADD COLUMN cache_probe TEXT; COMMIT",
    )?;

    connection.flush_prepared_statement_cache();

    {
        let mut statement = connection.prepare_cached(sql)?;
        assert_eq!(statement.get_status(StatementStatus::Run), 0);

        assert_eq!(
            statement.query_row([BUCKET], |row| row.get::<_, String>(0))?,
            "images"
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn cached_statement_failure_settles_before_reuse_and_fresh_reads() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = seed(&directory, &["a".into()])?;
    let sql = "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (?1, ?2, 0)";

    {
        let transaction = database.connection.transaction()?;

        {
            let mut statement = transaction.prepare_cached(sql)?;
            let error = statement.execute(params![[2u8; 16], "photos"]).unwrap_err();

            assert_eq!(
                error.sqlite_error_code(),
                Some(ErrorCode::ConstraintViolation)
            );
        }

        assert!(!transaction.is_autocommit());
        transaction.rollback()?;
    }

    assert!(database.connection.is_autocommit());

    {
        let transaction = database.connection.transaction()?;

        {
            let mut statement = transaction.prepare_cached(sql)?;
            assert_eq!(statement.get_status(StatementStatus::Run), 1);
            assert_eq!(statement.execute(params![[2u8; 16], "other"])?, 1);
        }

        transaction.commit()?;
    }

    let rows: i64 = database
        .connection
        .query_row("SELECT count(*) FROM buckets", [], |row| row.get(0))?;

    assert_eq!(rows, 2);
    close(database)?;
    directory.finish()?;
    Ok(())
}
