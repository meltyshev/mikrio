//! Representation round trips through the current schema and bundled SQLite.

use crate::object::{
    ObjectKey, ObjectTags, ObjectValueError, Representation, RepresentationInput, UserMetadata,
};

use super::*;

#[test]
fn accepted_representation_round_trips_and_invalid_stored_values_are_rejected() -> TestResult {
    let directory = TestDirectory::new()?;
    let key = ObjectKey::parse("a/../e\u{301}")?;
    let database = seed(&directory, &[key.as_str().to_owned()])?;
    let metadata = UserMetadata::new([("Quoted".into(), "\\".repeat(2042))])?;

    let representation = Representation::new(RepresentationInput {
        content_type: Some(String::new()),
        content_language: Some("en".into()),
        expires: Some(EpochMillis::new(-1)?),
        ..RepresentationInput::default()
    })?;

    let fields = representation.fields();

    database.connection.execute(
        "UPDATE objects SET content_type=?1, content_language=?2, expires_at_ms=?3,
         user_metadata_json=?4 WHERE bucket_id=?5 AND key=?6",
        params![
            fields.content_type,
            fields.content_language,
            fields.expires.map(EpochMillis::get),
            metadata.to_json()?,
            BUCKET,
            key.as_bytes()
        ],
    )?;

    let mut statement = database.connection.prepare(
        "SELECT key, content_type, content_encoding, content_disposition,
         content_language, cache_control, expires_at_ms, user_metadata_json
         FROM objects WHERE bucket_id=?1 AND key=?2",
    )?;

    let mut rows = statement.query(params![BUCKET, key.as_bytes()])?;
    let row = rows.next()?.unwrap();
    assert_eq!(ObjectKey::from_bytes(&row.get::<_, Vec<u8>>(0)?)?, key);

    let stored = Representation::new(RepresentationInput {
        content_type: row.get(1)?,
        content_encoding: row.get(2)?,
        content_disposition: row.get(3)?,
        content_language: row.get(4)?,
        cache_control: row.get(5)?,
        expires: row
            .get::<_, Option<i64>>(6)?
            .map(EpochMillis::new)
            .transpose()?,
    })?;

    assert_eq!(stored, representation);

    assert_eq!(
        UserMetadata::from_json(&row.get::<_, String>(7)?)?,
        metadata
    );

    assert!(rows.next()?.is_none());
    drop(rows);
    drop(statement);

    // SQL constrains the envelope; shared Rust validation owns its semantics.
    for invalid in [r#"{"name":"one","name":"two"}"#, r#"{"name":"\n"}"#] {
        database.connection.execute(
            "UPDATE objects SET user_metadata_json=?1 WHERE bucket_id=?2 AND key=?3",
            params![invalid, BUCKET, key.as_bytes()],
        )?;

        let encoded: String = database.connection.query_row(
            "SELECT user_metadata_json FROM objects WHERE bucket_id=?1 AND key=?2",
            params![BUCKET, key.as_bytes()],
            |row| row.get(0),
        )?;

        assert_eq!(
            UserMetadata::from_json(&encoded),
            Err(ObjectValueError::MetadataEncoding)
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn stored_content_type_distinguishes_null_empty_and_explicit_values() -> TestResult {
    let directory = TestDirectory::new()?;
    let database = seed(&directory, &["key".into()])?;

    for (supplied, expected) in [
        (None, "binary/octet-stream"),
        (Some(""), ""),
        (
            Some("application/x-mikrio-fixture"),
            "application/x-mikrio-fixture",
        ),
    ] {
        database.connection.execute(
            "UPDATE objects SET content_type=?1 WHERE bucket_id=?2 AND key=?3",
            params![supplied, BUCKET, b"key"],
        )?;

        let content_type: Option<String> = database.connection.query_row(
            "SELECT content_type FROM objects WHERE bucket_id=?1 AND key=?2",
            params![BUCKET, b"key"],
            |row| row.get(0),
        )?;

        assert_eq!(content_type.as_deref(), supplied);

        let stored = Representation::new(RepresentationInput {
            content_type,
            ..RepresentationInput::default()
        })?;

        assert_eq!(stored.effective_content_type(), expected);
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn stored_tags_round_trip_exact_values_and_reject_semantic_corruption() -> TestResult {
    let directory = TestDirectory::new()?;
    let database = seed(&directory, &["key".into()])?;

    let tags = ObjectTags::new(
        (0..10).map(|index| (format!("{index}{}", "日".repeat(127)), "𐐀".repeat(128))),
    )?;

    database.connection.execute(
        "UPDATE objects SET tags_json=?1 WHERE bucket_id=?2 AND key=?3",
        params![tags.to_json()?, BUCKET, b"key"],
    )?;

    close(database)?;

    let database = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let encoded: String = database.connection.query_row(
        "SELECT tags_json FROM objects WHERE bucket_id=?1 AND key=?2",
        params![BUCKET, b"key"],
        |row| row.get(0),
    )?;

    assert_eq!(ObjectTags::from_json(&encoded)?, tags);

    for invalid in [
        r#"{"name":"first","name":"last"}"#,
        r#"{"name":"\ud800"}"#,
        r#"{"name":"\n"}"#,
        r#"{"aws:name":"value"}"#,
    ] {
        database.connection.execute(
            "UPDATE objects SET tags_json=?1 WHERE bucket_id=?2 AND key=?3",
            params![invalid, BUCKET, b"key"],
        )?;

        let encoded: String = database.connection.query_row(
            "SELECT tags_json FROM objects WHERE bucket_id=?1 AND key=?2",
            params![BUCKET, b"key"],
            |row| row.get(0),
        )?;

        assert_eq!(
            ObjectTags::from_json(&encoded),
            Err(ObjectValueError::TagEncoding)
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
