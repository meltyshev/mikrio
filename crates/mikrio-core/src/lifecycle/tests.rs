use super::*;

mod configuration;

#[test]
fn provisional_upload_abort_midnight_fixtures_use_the_original_initiation() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../../../tests/contract/lifecycle.json")).unwrap();

    for case in fixtures["abort_incomplete_multipart"].as_array().unwrap() {
        let anchor = EpochMillis::new(case["anchor_ms"].as_i64().unwrap()).unwrap();
        let days = u32::try_from(case["days"].as_u64().unwrap()).unwrap();
        let expected = EpochMillis::new(case["expected_ms"].as_i64().unwrap()).unwrap();
        assert_eq!(abort_at(anchor, days), Ok(expected), "{case}");
    }

    assert_eq!(
        abort_at(EpochMillis::new(0).unwrap(), 0),
        Err(LifecycleError::Days)
    );

    assert_eq!(
        abort_at(EpochMillis::new(253_402_300_799_999).unwrap(), 1),
        Err(LifecycleError::Time)
    );
}

#[test]
fn published_and_independent_midnight_fixtures_keep_the_original_age() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../../../tests/contract/lifecycle.json")).unwrap();

    for case in fixtures["object_expiration"].as_array().unwrap() {
        let anchor = EpochMillis::new(case["anchor_ms"].as_i64().unwrap()).unwrap();
        let days = u32::try_from(case["days"].as_u64().unwrap()).unwrap();
        let expected = EpochMillis::new(case["expected_ms"].as_i64().unwrap()).unwrap();
        assert_eq!(expiration_at(anchor, days), Ok(expected), "{case}");
    }
}

#[test]
fn midnight_edges_pre_epoch_dates_and_leap_days_do_not_use_local_time() {
    for (anchor, days, expected) in [
        (-1, 1, 86_400_000),
        (0, 1, 172_800_000),
        (1, 1, 172_800_000),
        (86_399_999, 1, 172_800_000),
        (86_400_000, 1, 259_200_000),
        // 2024-02-28 noon + one day rounds past leap day to March 1.
        (1_709_121_600_000, 1, 1_709_251_200_000),
    ] {
        assert_eq!(
            expiration_at(EpochMillis::new(anchor).unwrap(), days),
            Ok(EpochMillis::new(expected).unwrap()),
            "anchor {anchor}, days {days}"
        );
    }
}

#[test]
fn invalid_days_and_unrepresentable_deadlines_fail_without_wrapping() {
    let epoch = EpochMillis::new(0).unwrap();

    for days in [0, 2_147_483_648, u32::MAX] {
        assert_eq!(
            expiration_at(epoch, days),
            Err(LifecycleError::Days),
            "days={days}"
        );
    }

    for (anchor, days) in [(253_402_300_799_999, 1), (0, 2_147_483_647)] {
        assert_eq!(
            expiration_at(EpochMillis::new(anchor).unwrap(), days),
            Err(LifecycleError::Time),
            "anchor={anchor}, days={days}"
        );
    }

    assert_eq!(
        expiration_at(EpochMillis::new(-62_135_596_800_000).unwrap(), 1),
        Ok(EpochMillis::new(-62_135_424_000_000).unwrap())
    );
}
