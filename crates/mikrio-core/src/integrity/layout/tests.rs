use super::*;

fn part(part_number: u32, byte_offset: u64, byte_length: u64) -> PartLayout {
    PartLayout {
        part_number,
        byte_offset,
        byte_length,
        checksums: Vec::new(),
    }
}

#[test]
fn sparse_numbers_preserve_exact_byte_boundaries_and_an_empty_final_part() {
    let expected = vec![
        part(2, 0, 5_242_880),
        part(9, 5_242_880, 6_291_456),
        part(10_000, 11_534_336, 0),
    ];

    let layout = MultipartLayout::new(expected.clone(), 11_534_336).unwrap();
    assert_eq!(layout.parts(), expected);
    assert_eq!(layout.part_count(), 3);
    assert_eq!(layout.content_length(), 11_534_336);

    for length in [0, 1, 5_242_879, 5_242_880, 5_368_709_120] {
        let layout = MultipartLayout::new([part(10_000, 0, length)], length).unwrap();
        assert_eq!(layout.part_count(), 1, "length={length}");
        assert_eq!(layout.content_length(), length);
        assert_eq!(layout.parts()[0].part_number, 10_000, "length={length}");
    }
}

#[test]
fn invalid_counts_numbers_sizes_and_byte_layouts_are_rejected() {
    assert_eq!(MultipartLayout::new([], 0), Err(LayoutError::PartCount));

    for number in [0, 10_001, u32::MAX] {
        assert_eq!(
            MultipartLayout::new([part(number, 0, 0)], 0),
            Err(LayoutError::PartNumber),
            "number={number}"
        );
    }

    for number in [1, 2] {
        assert_eq!(
            MultipartLayout::new(
                [part(2, 0, 5_242_880), part(number, 5_242_880, 0)],
                5_242_880
            ),
            Err(LayoutError::PartNumber),
            "number={number}"
        );
    }

    for length in [0, 1, 5_242_879] {
        assert_eq!(
            MultipartLayout::new([part(1, 0, length), part(2, length, 1)], length + 1),
            Err(LayoutError::PartSize),
            "length={length}"
        );
    }

    for length in [5_368_709_121, u64::MAX] {
        assert_eq!(
            MultipartLayout::new([part(1, 0, length)], length),
            Err(LayoutError::PartSize),
            "length={length}"
        );
    }

    for offset in [0, 5_242_879, 5_242_881, u64::MAX] {
        assert_eq!(
            MultipartLayout::new([part(1, 0, 5_242_880), part(2, offset, 1)], 5_242_881),
            Err(LayoutError::ByteLayout),
            "offset={offset}"
        );
    }

    for offset in [1, u64::MAX] {
        assert_eq!(
            MultipartLayout::new([part(1, offset, 0)], 0),
            Err(LayoutError::ByteLayout),
            "offset={offset}"
        );
    }

    for length in [0, 2, u64::MAX] {
        assert_eq!(
            MultipartLayout::new([part(1, 0, 1)], length),
            Err(LayoutError::ByteLayout),
            "length={length}"
        );
    }
}

#[test]
fn typed_digests_retain_all_algorithms_but_reject_duplicate_algorithm_facts() {
    let checksums = vec![
        Checksum::Crc32([1; 4]),
        Checksum::Crc32c([2; 4]),
        Checksum::Crc64Nvme([3; 8]),
        Checksum::Sha1([4; 20]),
        Checksum::Sha256([5; 32]),
    ];

    let mut candidate = part(1, 0, 0);
    candidate.checksums.clone_from(&checksums);
    let layout = MultipartLayout::new([candidate.clone()], 0).unwrap();
    assert_eq!(layout.parts()[0].checksums, checksums);

    for checksum in checksums {
        candidate.checksums = vec![checksum, checksum];

        assert_eq!(
            MultipartLayout::new([candidate.clone()], 0),
            Err(LayoutError::Checksums),
            "checksum={checksum:?}"
        );
    }

    candidate.checksums = vec![Checksum::Crc32([0; 4]); 6];

    assert_eq!(
        MultipartLayout::new([candidate], 0),
        Err(LayoutError::Checksums)
    );
}

#[test]
fn maximum_layout_is_bounded_and_overlong_input_stops_at_one_lookahead() {
    // Metadata-only boundary, not a 48.8 TiB transfer or durability run.
    let selected = (1..=10_000)
        .map(|number| part(number, u64::from(number - 1) * 5_368_709_120, 5_368_709_120));

    let layout = MultipartLayout::new(selected, 53_687_091_200_000).unwrap();
    assert_eq!(layout.part_count(), 10_000);
    assert_eq!(layout.parts().len(), 10_000);
    assert_eq!(layout.content_length(), 53_687_091_200_000);
    assert_eq!(layout.parts()[9_999].byte_offset, 53_681_722_490_880);

    let mut consumed = 0_u32;

    let overlong = std::iter::from_fn(|| {
        consumed += 1;

        Some(part(
            consumed,
            u64::from(consumed - 1) * 5_368_709_120,
            5_368_709_120,
        ))
    });

    assert_eq!(
        MultipartLayout::new(overlong, u64::MAX),
        Err(LayoutError::PartCount)
    );

    assert_eq!(consumed, 10_001);
}
