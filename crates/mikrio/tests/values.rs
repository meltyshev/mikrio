use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mikrio::{ErrorCode, Timestamp};

#[test]
fn timestamp_preserves_shared_bounds_and_rejects_overflow() {
    for milliseconds in [-62_135_596_800_000, -1, 0, 253_402_300_799_999] {
        assert_eq!(
            Timestamp::from_unix_millis(milliseconds)
                .unwrap()
                .as_unix_millis(),
            milliseconds
        );
    }

    for milliseconds in [i64::MIN, -62_135_596_800_001, 253_402_300_800_000, i64::MAX] {
        let error = Timestamp::from_unix_millis(milliseconds).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument, "{milliseconds}");

        assert!(
            std::error::Error::source(&error).is_some(),
            "{milliseconds}"
        );
    }
}

#[test]
fn timestamp_clock_conversions_preserve_millisecond_precision() {
    let instant = UNIX_EPOCH.checked_sub(Duration::from_nanos(100)).unwrap();
    let timestamp = Timestamp::try_from(instant).unwrap();
    assert_eq!(timestamp.as_unix_millis(), -1);

    assert_eq!(
        SystemTime::try_from(timestamp).unwrap(),
        UNIX_EPOCH.checked_sub(Duration::from_millis(1)).unwrap()
    );

    let maximum = Timestamp::from_unix_millis(253_402_300_799_999).unwrap();

    assert_eq!(
        Timestamp::try_from(SystemTime::try_from(maximum).unwrap()).unwrap(),
        maximum
    );
}
