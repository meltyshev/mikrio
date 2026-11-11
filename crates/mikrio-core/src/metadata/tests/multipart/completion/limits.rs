use super::*;
use std::io::Write as _;

#[test]
fn maximum_part_metadata_selection_claim_and_retirement_keep_the_exact_atomic_set() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let (mut request, facts) = fixture(&mut database)?;

    // Synthetic metadata only: no large payload is created or claimed verified.
    // The independent capacity/durability lane keeps valid physical part bytes.
    let transaction = database.connection.transaction()?;

    transaction.execute(
        "UPDATE blobs SET byte_length = ?1",
        [i64::try_from(crate::multipart::PART_MAX_BYTES)?],
    )?;

    let mut blob_insert = transaction.prepare(
        "INSERT INTO blobs (blob_id, shard, state, write_kind, byte_length, etag, md5_digest,
         checksum_type, checksums_json, created_at_ms, gc_pending, gc_attempt_count)
         SELECT ?1, ?2, state, write_kind, byte_length, etag, md5_digest, checksum_type,
         checksums_json, created_at_ms, 0, 0 FROM blobs WHERE blob_id = ?3",
    )?;

    let mut part_insert = transaction.prepare(
        "INSERT INTO multipart_parts (upload_id,part_number,generation,blob_id,last_modified_at_ms)
         VALUES (?1,?2,?3,?3,1000)",
    )?;

    for number in 2_u32..=10_000 {
        let mut blob = [0_u8; 16];
        blob[0] = number.to_be_bytes()[3];
        blob[12..].copy_from_slice(&number.to_be_bytes());

        blob_insert.execute(rusqlite::params![
            blob.as_slice(),
            i64::from(blob[0]),
            [34_u8; 16].as_slice()
        ])?;

        part_insert.execute(rusqlite::params![
            request.upload.upload_id.to_string(),
            i64::from(number),
            blob.as_slice()
        ])?;
    }

    drop(part_insert);
    drop(blob_insert);
    transaction.commit()?;

    request.parts = CompletionParts::new((1..=10_000).map(|number| SelectedPart {
        part_number: PartNumber::new(number).unwrap(),
        etag: facts.etag(),
        checksum: Some(facts.checksum()),
    }))?;

    database.connection.flush_prepared_statement_cache();
    let started = std::time::Instant::now();
    let selected = database.select_completion(&Authority::Native, &request)?;
    let selection_us = started.elapsed().as_micros();
    let selection_runs = crate::metadata::multipart::part_selection_work(&database.connection)?;

    assert!(
        (1..=2).contains(&selection_runs),
        "selection ran {selection_runs} part queries"
    );

    assert_eq!(selected.layout.content_length(), 53_687_091_200_000);
    assert_eq!(selected.layout.part_count(), 10_000);
    let started = std::time::Instant::now();
    assert!(database.claim_completion(&Authority::Native, &request, &selected, claim(60))?);
    let claim_us = started.elapsed().as_micros();

    let claim_runs =
        crate::metadata::multipart::part_selection_work(&database.connection)? - selection_runs;

    assert!(
        (1..=2).contains(&claim_runs),
        "claim ran {claim_runs} part queries"
    );

    let started = std::time::Instant::now();
    database.abort_multipart_upload(&Authority::Native, &request.upload)?;
    let abort_us = started.elapsed().as_micros();

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM multipart_parts", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );

    assert_eq!(
        database.connection.query_row(
            "SELECT count(*) FROM blobs WHERE gc_pending=1",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        10_001
    );

    writeln!(
        std::io::stderr().lock(),
        "synthetic 10000-part metadata: selection_us={selection_us} claim_us={claim_us} abort_us={abort_us} selection_runs={selection_runs} claim_runs={claim_runs}"
    )?;

    close(database)?;
    directory.finish()?;

    Ok(())
}
