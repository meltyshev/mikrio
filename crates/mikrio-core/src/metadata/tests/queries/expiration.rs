//! Production presence/raw-page queries on synthetic metadata without payload files.

use super::*;
use crate::access::BucketAccessRequest;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use crate::object::{Authority, BucketName};

#[test]
#[expect(
    clippy::print_stderr,
    reason = "qualification retains the bounded SQL observation"
)]
fn sparse_lifecycle_pages_bound_raw_work_without_searching_for_a_matching_object() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let keys = (0..100_000)
        .map(|index| format!("key/{index:06}"))
        .collect::<Vec<_>>();

    let mut database = seed(&directory, &keys)?;

    database.put_bucket_lifecycle(
        &Authority::Native,
        &BucketAccessRequest {
            bucket: BucketName::parse("photos")?,
            expected_owner: None,
        },
        &LifecycleConfiguration::new(vec![LifecycleRule {
            id: Some("sparse".into()),
            is_enabled: true,
            filter: LifecycleFilter::Prefix("key/099999".into()),
            expiration_days: Some(1),
            abort_incomplete_multipart_days: None,
        }])?,
    )?;

    let started = Instant::now();

    for (cursor, examined) in [(0, 16), (16, 32)] {
        let page = database
            .object_expiration_page(EpochMillis::new(172_800_000)?)?
            .unwrap();

        assert_eq!(page.progress.cursor, cursor);
        assert_eq!(page.progress.examined, examined);
        assert_eq!(page.progress.upper, 100_000);
        assert_eq!(page.candidates.len(), 16);

        assert!(
            page.candidates
                .iter()
                .all(|(_, candidate)| matches!(candidate, Ok(None)))
        );

        database.advance_sweep(page.progress)?;
    }

    let steps = crate::metadata::lifecycle::expiration::indexed_work(&database.connection)?;

    assert!(
        steps > 0 && steps < 2_000,
        "unexpected presence/page SQL work: {steps}"
    );

    eprintln!(
        "EXPIRATION_MEASUREMENT rows=100000 raw_pages=2 examined=32 presence_page_vm_steps={steps} elapsed_us={}",
        started.elapsed().as_micros()
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
