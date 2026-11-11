use super::*;

#[test]
fn single_ranges_resolve_with_clipping_without_arithmetic_overflow() {
    let length = ByteLength::new(11).unwrap();

    for (input, first, last) in [
        ("bytes=4-7", 4, 7),
        ("bytes=4-", 4, 10),
        ("bytes=-7", 4, 10),
        ("bytes=0-0", 0, 0),
        ("bytes=4-999", 4, 10),
        ("bytes=-999", 0, 10),
        ("bytes=0-18446744073709551615", 0, 10),
        ("bytes=-18446744073709551615", 0, 10),
        ("bytes=0004-0007", 4, 7),
    ] {
        let range = ByteRange::parse(input).unwrap();
        assert_eq!(range.as_str(), input);
        let selected = range.select(length).unwrap();
        assert_eq!(selected.offset(), first, "{input}");
        assert_eq!(selected.length(), last - first + 1, "{input}");

        assert_eq!(
            selected.content_range(length),
            format!("bytes {first}-{last}/11"),
            "{input}"
        );
    }
}

#[test]
fn malformed_syntax_is_distinct_from_an_unsatisfiable_selection() {
    for input in [
        "",
        "bytes=",
        "bytes=-",
        "bytes=+1-",
        "bytes=1-0",
        "bytes=1-2,4-5",
        "items=0-1",
        "bytes= 0-1",
        "bytes=0-1 ",
        "bytes=0--1",
        "bytes=0-18446744073709551616",
        "bytes=-+1",
    ] {
        assert_eq!(
            ByteRange::parse(input).unwrap_err(),
            ObjectValueError::ReadRange,
            "{input}"
        );
    }

    for input in [
        "bytes=11-",
        "bytes=40-50",
        "bytes=-0",
        "bytes=18446744073709551615-",
    ] {
        assert_eq!(
            ByteRange::parse(input)
                .unwrap()
                .select(ByteLength::new(11).unwrap())
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidRange,
            "{input}"
        );
    }

    for input in ["bytes=0-", "bytes=0-0", "bytes=-1", "bytes=-0"] {
        assert_eq!(
            ByteRange::parse(input)
                .unwrap()
                .select(ByteLength::new(0).unwrap())
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidRange,
            "{input}"
        );
    }
}
