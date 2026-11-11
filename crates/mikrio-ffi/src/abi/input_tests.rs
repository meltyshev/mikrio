//! Layout tests use independent canonical byte counts, including every rejected
//! partial field. Installed C consumers additionally check inaccessible tails.

use std::mem::{align_of, size_of};
use std::ptr;

use super::{METADATA_LIMITS, Pair, SizedInput, Span, TAG_LIMITS, pairs, sized};

fn check_sizes<T: SizedInput>(accepted: &[usize]) {
    assert!(align_of::<T>() <= align_of::<u64>());

    let mut storage = vec![0_u64; 8193];
    let input = storage.as_mut_ptr().cast::<T>();

    for length in (0..=size_of::<T>() + 1).chain([65_536, 65_537, usize::MAX]) {
        storage[0] = u64::try_from(length).unwrap();
        // SAFETY: aligned initialized allocation contains every accepted size;
        // rejected excessive sizes require only the readable leading word.
        let decoded = unsafe { sized(input) };
        let expected = accepted.contains(&length) || (length > size_of::<T>() && length <= 65_536);

        assert_eq!(
            decoded.is_ok(),
            expected,
            "{} length {length}",
            std::any::type_name::<T>()
        );

        if let Err(error) = decoded {
            assert_eq!(
                error.code,
                "InvalidArgument",
                "{} length {length}",
                std::any::type_name::<T>()
            );
        }
    }
}

#[test]
fn input_sizes_reject_partial_fields_and_required_suffixes() {
    use super::access_configuration::*;
    use super::admin::*;
    use super::cors::*;
    use super::deletion::DeleteObjectsInput;
    use super::lifecycle::*;
    use super::listener::ListenOptions;
    use super::listing::{ListBucketsInput, ListObjectsInput};
    use super::multipart::*;
    use super::object_attributes::GetObjectAttributesInput;
    use super::object_tagging::{
        DeleteObjectTaggingInput, GetObjectTaggingInput, PutObjectTaggingInput,
    };
    use super::{BucketInput, DeleteInput, OpenOptions, PutInput, ReadInput};

    check_sizes::<OpenOptions>(&[24, 40, 56]);
    check_sizes::<ListenOptions>(&[8, 24, 40, 56, 88]);
    check_sizes::<BucketInput>(&[24, 40]);

    check_sizes::<PutInput>(&[
        40, 56, 72, 88, 104, 120, 136, 152, 168, 184, 200, 216, 232, 248, 264, 280, 296, 312, 328,
        344,
    ]);

    check_sizes::<ReadInput>(&[
        40, 56, 72, 88, 104, 120, 136, 152, 168, 184, 200, 216, 232, 248, 264,
    ]);

    check_sizes::<DeleteInput>(&[40, 56, 72]);
    check_sizes::<ListBucketsInput>(&[8, 24, 40, 56]);
    check_sizes::<ListObjectsInput>(&[24, 40, 56, 72, 88, 104, 120]);
    check_sizes::<DeleteObjectsInput>(&[56, 64]);
    check_sizes::<CreatePrincipalInput>(&[24]);
    check_sizes::<GetPrincipalInput>(&[24]);
    check_sizes::<SetPrincipalEnabledInput>(&[32]);
    check_sizes::<DeletePrincipalInput>(&[24]);
    check_sizes::<PutPrincipalPolicyInput>(&[40]);
    check_sizes::<GetPrincipalPolicyInput>(&[24]);
    check_sizes::<DeletePrincipalPolicyInput>(&[24]);
    check_sizes::<CreateAccessKeyInput>(&[24]);
    check_sizes::<DeleteAccessKeyInput>(&[40]);
    check_sizes::<ListPrincipalsInput>(&[8, 24, 40]);
    check_sizes::<ListAccessKeysInput>(&[24, 40, 56]);
    check_sizes::<PutBucketPolicyInput>(&[56]);
    check_sizes::<GetBucketPolicyInput>(&[24, 40]);
    check_sizes::<DeleteBucketPolicyInput>(&[24, 40]);
    check_sizes::<GetBucketPolicyStatusInput>(&[24, 40]);
    check_sizes::<PutPublicAccessBlockInput>(&[56]);
    check_sizes::<GetPublicAccessBlockInput>(&[24, 40]);
    check_sizes::<DeletePublicAccessBlockInput>(&[24, 40]);
    check_sizes::<PutBucketOwnershipControlsInput>(&[48]);
    check_sizes::<GetBucketOwnershipControlsInput>(&[24, 40]);
    check_sizes::<GetBucketAclInput>(&[24, 40]);
    check_sizes::<GetObjectAclInput>(&[40, 56]);
    check_sizes::<PutBucketLifecycleConfigurationInput>(&[56]);
    check_sizes::<GetBucketLifecycleConfigurationInput>(&[24, 40]);
    check_sizes::<DeleteBucketLifecycleInput>(&[24, 40]);
    check_sizes::<PutBucketCorsInput>(&[56]);
    check_sizes::<GetBucketCorsInput>(&[24, 40]);
    check_sizes::<DeleteBucketCorsInput>(&[24, 40]);
    check_sizes::<GetObjectAttributesInput>(&[72, 88, 104]);
    check_sizes::<GetObjectTaggingInput>(&[40, 56]);
    check_sizes::<PutObjectTaggingInput>(&[72]);
    check_sizes::<DeleteObjectTaggingInput>(&[40, 56]);
    check_sizes::<CopyObjectInput>(&[376]);
    check_sizes::<CreateMultipartUploadInput>(&[216]);
    check_sizes::<UploadPartInput>(&[208]);
    check_sizes::<UploadPartCopyInput>(&[208]);
    check_sizes::<CompleteMultipartUploadInput>(&[232]);
    check_sizes::<AbortMultipartUploadInput>(&[72]);
    check_sizes::<ListPartsInput>(&[104]);
    check_sizes::<ListMultipartUploadsInput>(&[120]);
}

#[test]
fn multipart_value_layouts_match_the_c_contract() {
    use super::multipart::*;

    fn check<T>(expected: usize) {
        assert_eq!(
            size_of::<T>(),
            expected,
            "{} size",
            std::any::type_name::<T>()
        );
        assert_eq!(
            align_of::<T>(),
            8,
            "{} alignment",
            std::any::type_name::<T>()
        );
    }

    // These array strides and by-value returns are fixed for ABI major 1.
    // The independently compiled C++ consumer checks the same numeric contract.
    check::<ObjectRef>(32);
    check::<CompletedPart>(104);
    check::<CopyObjectOutputView>(104);
    check::<CreateMultipartUploadOutputView>(48);
    check::<UploadPartOutputView>(96);
    check::<UploadPartCopyOutputView>(104);
    check::<CompleteMultipartUploadOutputView>(112);
    check::<PartView>(120);
    check::<MultipartUploadView>(72);
    check::<ListPartsOutputView>(64);
    check::<ListMultipartUploadsOutputView>(56);
}

#[test]
fn omitted_options_are_zeroed_without_reading_the_callers_suffix() {
    let input = super::OpenOptions {
        struct_size: 24,
        directory: Span::borrowed("directory"),
        region: Span::borrowed("suffix must not be consumed"),
        create_if_missing: super::OptionalU64 {
            present: 1,
            reserved: 0,
            value: 2,
        },
    };

    // SAFETY: initialized aligned input with a complete 24-byte prefix.
    let decoded = unsafe { sized(&raw const input) }.unwrap();

    assert_eq!(decoded.directory.data, input.directory.data);
    assert_eq!(decoded.directory.len, 9);
    assert!(decoded.region.data.is_null());
    assert_eq!(decoded.region.len, 0);
    assert_eq!(decoded.create_if_missing.get().unwrap(), None);
}

#[test]
fn unknown_nonzero_extensions_and_null_inputs_are_rejected() {
    let mut storage = vec![0_u64; 8192];
    storage[0] = 65_536;
    let input = storage.as_ptr().cast::<super::OpenOptions>();

    // SAFETY: aligned initialized allocation includes the complete extension.
    assert!(unsafe { sized(input) }.is_ok());

    storage[8191] = 1;
    let input = storage.as_ptr().cast::<super::OpenOptions>();

    // SAFETY: same complete live allocation, with a nonzero final extension byte.
    let error = unsafe { sized(input) }
        .err()
        .expect("nonzero extension rejects");

    assert_eq!(error.code, "InvalidArgument");

    // SAFETY: null is explicitly checked without dereferencing it.
    let error = unsafe { sized::<super::OpenOptions>(ptr::null()) }
        .err()
        .expect("null input rejects");

    assert_eq!(error.code, "InvalidArgument");
}

#[test]
fn a_synthetic_future_layout_preserves_the_established_prefix_defaults() {
    #[repr(C)]
    struct EarlierOptions {
        struct_size: u64,
        directory: Span,
        region: Span,
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct FutureOptions {
        struct_size: u64,
        directory: Span,
        region: Span,
        future_region: Span,
    }

    input_layout!(FutureOptions, region, future_region);

    let original = EarlierOptions {
        struct_size: 40,
        directory: Span::borrowed("directory"),
        region: Span::borrowed("eu-west-1"),
    };

    // SAFETY: the earlier input has the same initialized 40-byte prefix and
    // alignment, but no future suffix allocation. The decoder must not read it.
    let decoded = unsafe { sized((&raw const original).cast::<FutureOptions>()) }.unwrap();

    assert_eq!(decoded.region.data, original.region.data);
    assert_eq!(decoded.region.len, 9);
    assert!(decoded.future_region.data.is_null());
    assert_eq!(decoded.future_region.len, 0);
}

#[test]
fn pair_byte_budgets_reject_oversized_and_aggregate_inputs_before_copying() {
    let large = "x".repeat(16 * 1024);
    let first = "a".repeat(1023);
    let second = "b".repeat(1024);

    let oversized = [Pair {
        key: Span::borrowed("key"),
        value: Span::borrowed(&large),
    }];

    let aggregate = [
        Pair {
            key: Span::borrowed("a"),
            value: Span::borrowed(&first),
        },
        Pair {
            key: Span::borrowed("b"),
            value: Span::borrowed(&second),
        },
    ];

    for (case, entries, limits) in [
        (
            "oversized metadata value",
            oversized.as_slice(),
            METADATA_LIMITS,
        ),
        (
            "aggregate metadata bytes",
            aggregate.as_slice(),
            METADATA_LIMITS,
        ),
        ("oversized tag value", oversized.as_slice(), TAG_LIMITS),
    ] {
        let count = u64::try_from(entries.len()).unwrap();
        // SAFETY: every pair and complete declared UTF-8 allocation stays live.
        let error = unsafe { pairs(entries.as_ptr(), count, limits) }.expect_err(case);

        assert_eq!(error.code, "InvalidArgument", "{case}");
    }
}

#[test]
fn pair_copy_accepts_full_utf8_budgets_and_rejects_the_metadata_entry_overflow() {
    let value = "é".repeat(1022);

    let metadata = [Pair {
        key: Span::borrowed("name"),
        value: Span::borrowed(&value),
    }];

    // SAFETY: one complete pair and its UTF-8 buffers remain live for the call.
    let copied = unsafe { pairs(metadata.as_ptr(), 1, METADATA_LIMITS) }.unwrap();

    assert_eq!(copied, [("name".into(), value)]);

    let keys: Vec<_> = (0..65).map(|index| format!("key-{index}")).collect();

    let entries: Vec<_> = keys
        .iter()
        .map(|key| Pair {
            key: Span::borrowed(key),
            value: Span::borrowed(""),
        })
        .collect();

    // SAFETY: both the 64-entry prefix and 65-entry array have live backing storage.
    let copied = unsafe { pairs(entries.as_ptr(), 64, METADATA_LIMITS) }.unwrap();

    assert_eq!(copied.len(), 64);

    // SAFETY: same complete 65-entry allocation; rejection does not rely on a bad pointer.
    let error = unsafe { pairs(entries.as_ptr(), 65, METADATA_LIMITS) }.unwrap_err();

    assert_eq!(error.code, "InvalidArgument");

    let key = "雪".repeat(128);
    let value = "雪".repeat(256);

    let tags = [Pair {
        key: Span::borrowed(&key),
        value: Span::borrowed(&value),
    }; 10];

    // SAFETY: ten complete pairs borrow initialized UTF-8 buffers for this call.
    let copied = unsafe { pairs(tags.as_ptr(), 10, TAG_LIMITS) }.unwrap();

    assert_eq!(copied, vec![(key, value); 10]);
    // This allocation check deliberately leaves duplicate/tag semantics to core.
}
