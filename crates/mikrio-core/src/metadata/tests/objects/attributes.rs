use super::*;
use crate::OperationError;
use crate::multipart::PartPageSize;
use crate::object::{Attribute, AttributeSelection, AttributesRequest, Authority, ByteLength};
use rusqlite::params;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const GROUPS: [Attribute; 5] = [
    Attribute::Etag,
    Attribute::Checksum,
    Attribute::ObjectParts,
    Attribute::StorageClass,
    Attribute::ObjectSize,
];
const MIN: u64 = 5 * 1024 * 1024;

fn input(put: &PutRequest) -> AttributesRequest {
    AttributesRequest {
        bucket: put.bucket.clone(),
        key: put.key.clone(),
        expected_owner: None,
        attributes: AttributeSelection::new(GROUPS).unwrap(),
        max_parts: PartPageSize::default(),
        part_number_marker: 0,
    }
}

fn ordinary(database: &mut MetadataDatabase, request: &PutRequest) -> TestResult {
    let blob = BlobId::from_bytes([24; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, request, blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        request,
        bucket,
        blob,
        &facts(request, b"abc")?,
    )?;

    Ok(())
}

fn layout(database: &mut MetadataDatabase, numbers: &[u32], last: u64) -> TestResult {
    // Synthetic SQL-only layout fixtures; no payload/durability claim.
    let count = numbers.len();
    let total = (u64::try_from(count)? - 1) * MIN + last;
    let tx = database.connection.transaction()?;

    tx.execute(
        "UPDATE blobs SET byte_length=?1, part_count=?2, etag=?3,
         write_kind='complete_multipart_upload'",
        params![
            i64::try_from(total)?,
            i64::try_from(count)?,
            format!("\"0123456789abcdef0123456789abcdef-{count}\"")
        ],
    )?;

    for (index, number) in numbers.iter().enumerate() {
        tx.execute(
            "INSERT INTO blob_parts(blob_id,part_number,byte_offset,byte_length,checksums_json,part_index)
          VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                [24_u8; 16].as_slice(),
                number,
                i64::try_from(u64::try_from(index)? * MIN)?,
                i64::try_from(if index + 1 == count { last } else { MIN })?,
                r#"{"CRC64NVME":"BeXKuz/B+us="}"#,
                u32::try_from(index + 1)?
            ],
        )?;
    }

    tx.commit()?;
    Ok(())
}

#[test]
fn attribute_selection_returns_only_requested_groups_and_rejects_invalid_inputs() -> TestResult {
    assert_eq!(
        AttributeSelection::new([]).unwrap_err().failure(),
        OperationFailure::InvalidArgument
    );

    for group in GROUPS {
        assert_eq!(
            AttributeSelection::new([group, group])
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidArgument
        );
    }

    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, put) = setup(&directory)?;
    ordinary(&mut database, &put)?;
    let mut request = input(&put);

    for mask in 1..32 {
        request.attributes = AttributeSelection::new(
            GROUPS
                .into_iter()
                .enumerate()
                .filter_map(|(index, group)| (mask & (1 << index) != 0).then_some(group)),
        )?;

        let result = database.select_attributes(&Authority::Native, &request)?;
        assert_eq!(result.etag.is_some(), mask & 1 != 0);
        assert_eq!(result.checksum.is_some(), mask & 2 != 0);
        assert!(result.object_parts.is_none());
        assert_eq!(result.storage_class, (mask & 8 != 0).then_some("STANDARD"));

        assert_eq!(
            result.object_size.map(ByteLength::get),
            (mask & 16 != 0).then_some(3)
        );
    }

    for (marker, size) in [(10_001, 1000), (0, 0)] {
        request.part_number_marker = marker;
        request.max_parts = PartPageSize::new(size)?;

        assert_eq!(
            database
                .select_attributes(&Authority::Native, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidArgument
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

fn raw_page(
    database: &mut MetadataDatabase,
    request: &AttributesRequest,
) -> Result<crate::object::StoredPartPage, OperationError> {
    use crate::metadata::objects::{layout, object};

    let selected = database.select_object(
        &Authority::Native,
        &ReadRequest {
            bucket: request.bucket.clone(),
            key: request.key.clone(),
            expected_owner: None,
            conditions: ReadConditions::default(),
            range: None,
            part_number: None,
            response: Representation::default(),
        },
    )?;

    database.read(|tx| {
        let selected = object(tx, selected.bucket_id, request.key.as_bytes())?.unwrap();

        layout::page(
            tx,
            &selected,
            request.part_number_marker,
            request.max_parts.get(),
        )?
        .ok_or_else(|| OperationError::new(OperationFailure::StoreCorrupt))
    })
}

#[test]
fn completed_layout_pages_preserve_sparse_numbers_offsets_and_exhaustion() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, put) = setup(&directory)?;
    ordinary(&mut database, &put)?;
    layout(&mut database, &[2, 5, 8], 0)?;
    let mut request = input(&put);

    for (marker, size, numbers, next) in [
        (0, 2, vec![2, 5], Some(5)),
        (1, 3, vec![2, 5, 8], None),
        (2, 1, vec![5], Some(5)),
        (4, 2, vec![5, 8], None),
        (5, 1, vec![8], None),
        (8, 1, vec![], None),
        (10_000, 1000, vec![], None),
    ] {
        request.part_number_marker = marker;
        request.max_parts = PartPageSize::new(size)?;
        let page = raw_page(&mut database, &request)?;
        assert_eq!(page.total_parts_count, 3);
        assert_eq!(page.next_part_number_marker, next);

        assert_eq!(
            page.parts
                .iter()
                .map(|part| part.part_number)
                .collect::<Vec<_>>(),
            numbers
        );

        for part in page.parts {
            let index = [2, 5, 8]
                .iter()
                .position(|number| *number == part.part_number)
                .unwrap();

            assert_eq!(part.byte_offset, u64::try_from(index)? * MIN);
            assert_eq!(part.byte_length, if index == 2 { 0 } else { MIN });
            assert_eq!(part.checksums[0].encode(), "BeXKuz/B+us=");
        }
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn one_empty_part_and_exact_page_boundaries_retain_the_immutable_total() -> TestResult {
    for (count, last, expected_next) in [(1, 0, None), (1000, 3, None), (1001, 3, Some(1000))] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let (mut database, put) = setup(&directory)?;
        ordinary(&mut database, &put)?;
        layout(&mut database, &(1..=count).collect::<Vec<_>>(), last)?;
        let request = input(&put);
        let page = raw_page(&mut database, &request)?;
        assert_eq!(page.total_parts_count, count);
        assert_eq!(page.parts.len(), usize::try_from(count.min(1000))?);
        assert_eq!(page.next_part_number_marker, expected_next);

        if count == 1 {
            assert_eq!(page.parts[0].byte_length, 0);

            database
                .connection
                .execute("UPDATE blobs SET byte_length=1", [])?;

            database
                .connection
                .execute("UPDATE blob_parts SET byte_offset=1", [])?;

            let mut exhausted = request;
            exhausted.part_number_marker = 1;

            assert_eq!(
                raw_page(&mut database, &exhausted).unwrap_err().failure(),
                OperationFailure::StoreCorrupt
            );
        }

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn observed_layout_corruption_cannot_become_a_short_or_empty_attribute_page() -> TestResult {
    for (statement, marker, size) in [
        ("DELETE FROM blob_parts", 0, 3),
        ("DELETE FROM blob_parts WHERE part_number=5", 0, 3),
        (
            "UPDATE blobs SET checksum_type='COMPOSITE', checksums_json='{\"SHA256\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=-3\"}'",
            0,
            1,
        ),
        (
            "UPDATE blob_parts SET part_index=4 WHERE part_number=8",
            5,
            1,
        ),
        (
            "UPDATE blob_parts SET part_index=4 WHERE part_number=5",
            0,
            1,
        ),
        (
            "UPDATE blob_parts SET byte_offset=1 WHERE part_number=2",
            0,
            1,
        ),
        (
            "UPDATE blob_parts SET byte_offset=1 WHERE part_number=5",
            0,
            1,
        ),
        (
            "UPDATE blob_parts SET byte_length=1 WHERE part_number=2",
            2,
            1,
        ),
        (
            "UPDATE blob_parts SET byte_length=0 WHERE part_number=8",
            8,
            1,
        ),
        (
            "UPDATE blob_parts SET checksums_json='{\"CRC64NVME\":\"bad\"}' WHERE part_number=5",
            2,
            1,
        ),
        (
            "UPDATE blob_parts SET checksums_json='{\"CRC64NVME\":\"BeXKuz/B+us=\",\"CRC64NVME\":\"BeXKuz/B+us=\"}' WHERE part_number=5",
            2,
            1,
        ),
        (
            "UPDATE blobs SET part_count=4,etag='\"0123456789abcdef0123456789abcdef-4\"'",
            0,
            3,
        ),
        (
            "UPDATE blobs SET part_count=1,etag='\"0123456789abcdef0123456789abcdef-1\"'",
            0,
            1,
        ),
        (
            "UPDATE blobs SET part_count=NULL,write_kind='put_object',etag='\"900150983cd24fb0d6963f7d28e17f72\"'",
            0,
            1,
        ),
    ] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let (mut database, put) = setup(&directory)?;
        ordinary(&mut database, &put)?;
        layout(&mut database, &[2, 5, 8], 3)?;
        database.connection.execute(statement, [])?;
        let mut request = input(&put);
        request.part_number_marker = marker;
        request.max_parts = PartPageSize::new(size)?;

        assert_eq!(
            raw_page(&mut database, &request).unwrap_err().failure(),
            OperationFailure::StoreCorrupt,
            "{statement}"
        );

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn maximum_metadata_layout_pages_use_indexed_bounded_selection() -> TestResult {
    use rusqlite::StatementStatus;
    use std::io::Write as _;
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, put) = setup(&directory)?;
    ordinary(&mut database, &put)?;
    layout(&mut database, &(1..=10_000).collect::<Vec<_>>(), 3)?;
    let mut request = input(&put);
    request.part_number_marker = 9000;
    let page = raw_page(&mut database, &request)?;
    assert_eq!(page.total_parts_count, 10_000);
    assert_eq!(page.parts.len(), 1000);
    assert_eq!(page.parts[0].part_number, 9001);
    assert_eq!(page.parts[999].part_number, 10_000);
    assert_eq!(page.next_part_number_marker, None);

    let statement = database
        .connection
        .prepare_cached(crate::metadata::objects::layout::PAGE)?;

    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
    assert!(statement.get_status(StatementStatus::VmStep) < 20_000);

    writeln!(
        std::io::stdout().lock(),
        "10000-part metadata page: {} VM steps, zero full-scan steps",
        statement.get_status(StatementStatus::VmStep)
    )?;

    drop(statement);
    let mut part = read(&put);
    part.part_number = Some(crate::multipart::PartNumber::new(9000)?);

    assert_eq!(
        database
            .select_object(&Authority::Native, &part)?
            .content_offset(),
        8999 * MIN
    );

    let statement = database
        .connection
        .prepare_cached(crate::metadata::objects::layout::PART)?;

    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
    assert!(statement.get_status(StatementStatus::VmStep) < 100);

    writeln!(
        std::io::stdout().lock(),
        "10000-part ordinal selection: {} VM steps, zero full-scan steps",
        statement.get_status(StatementStatus::VmStep)
    )?;

    drop(statement);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn part_selection_uses_assembly_positions_and_checks_local_layout() -> TestResult {
    for damage in [
        None,
        Some("DELETE FROM blob_parts WHERE part_number=5"),
        Some("UPDATE blob_parts SET byte_offset=1 WHERE part_number=5"),
        Some("UPDATE blob_parts SET part_index=4 WHERE part_number=8"),
    ] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let (mut database, put) = setup(&directory)?;
        ordinary(&mut database, &put)?;
        layout(&mut database, &[2, 5, 8], 0)?;
        let mut request = read(&put);
        request.part_number = Some(crate::multipart::PartNumber::new(2)?);

        if let Some(sql) = damage {
            database.connection.execute(sql, [])?;

            assert_eq!(
                database
                    .select_object(&Authority::Native, &request)
                    .unwrap_err()
                    .failure(),
                OperationFailure::StoreCorrupt,
                "{sql}"
            );
        } else {
            let selected = database.select_object(&Authority::Native, &request)?;
            assert_eq!(selected.content_offset(), MIN);
            assert_eq!(selected.content_length(), MIN);

            assert_eq!(
                selected.content_range().as_deref(),
                Some("bytes 5242880-10485759/10485760")
            );

            assert_eq!(selected.parts_count(), Some(3));
            assert!(selected.checksums.is_empty());
            request.part_number = Some(crate::multipart::PartNumber::new(3)?);
            let empty = database.select_object(&Authority::Native, &request)?;
            assert_eq!(empty.content_offset(), 2 * MIN);
            assert_eq!(empty.content_length(), 0);
            assert!(empty.content_range().is_none());
            request.part_number = Some(crate::multipart::PartNumber::new(4)?);

            let missing = database
                .select_object(&Authority::Native, &request)
                .unwrap_err();

            assert_eq!(missing.failure(), OperationFailure::InvalidPartNumber);
            assert_eq!(missing.part_counts(), Some((3, 4)));
        }

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn ordinary_and_count_only_parts_do_not_invent_layout_entries() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, put) = setup(&directory)?;
    ordinary(&mut database, &put)?;
    let mut request = read(&put);
    request.part_number = Some(crate::multipart::PartNumber::new(1)?);
    let selected = database.select_object(&Authority::Native, &request)?;
    assert_eq!(selected.content_range().as_deref(), Some("bytes 0-2/3"));
    assert_eq!(selected.parts_count(), None);
    assert_eq!(selected.checksums["CRC64NVME"], "BeXKuz/B+us=");
    request.range = Some(crate::object::ByteRange::parse("bytes=0-1")?);

    assert_eq!(
        database
            .select_object(&Authority::Native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidRequest
    );

    layout(&mut database, &[2, 7], 3)?;
    let attributes = database.select_attributes(&Authority::Native, &input(&put))?;
    let parts = attributes.object_parts.unwrap();
    assert_eq!(parts.total_parts_count, 2);
    assert!(!parts.entries_available);
    assert_eq!(parts.parts, []);
    assert_eq!(parts.next_part_number_marker, None);

    for query in [
        crate::metadata::objects::layout::PAGE,
        crate::metadata::objects::layout::PART,
    ] {
        let statement = database.connection.prepare_cached(query)?;
        assert_eq!(statement.get_status(rusqlite::StatementStatus::VmStep), 0);
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn composite_layout_rejects_nonconsecutive_original_numbers() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, put) = setup(&directory)?;
    ordinary(&mut database, &put)?;
    layout(&mut database, &[1, 2], 3)?;

    database.connection.execute_batch(
        "UPDATE blobs SET checksum_type='COMPOSITE',
         checksums_json='{\"SHA256\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=-2\"}';
         UPDATE blob_parts SET checksums_json='{\"SHA256\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"}';",
    )?;

    let mut selected = read(&put);
    selected.part_number = Some(crate::multipart::PartNumber::new(2)?);

    assert_eq!(
        database
            .select_object(&Authority::Native, &selected)?
            .content_length(),
        3
    );

    assert_eq!(
        database
            .select_attributes(&Authority::Native, &input(&put))?
            .object_parts
            .unwrap()
            .parts
            .len(),
        2
    );

    database.connection.execute(
        "UPDATE blob_parts SET part_number=7 WHERE part_number=2",
        [],
    )?;

    assert_eq!(
        database
            .select_attributes(&Authority::Native, &input(&put))
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    assert_eq!(
        database
            .select_object(&Authority::Native, &selected)
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
