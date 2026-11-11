//! Required checksum and mode pairs cover empty and multichunk part replacements.

use super::*;

#[test]
fn crc32_full_object_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Crc32,
        "y/Q5Jg==",
        ChecksumType::FullObject,
    )
}

#[test]
fn crc32_composite_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Crc32,
        "y/Q5Jg==",
        ChecksumType::Composite,
    )
}

#[test]
fn crc32c_full_object_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Crc32c,
        "4waSgw==",
        ChecksumType::FullObject,
    )
}

#[test]
fn crc32c_composite_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Crc32c,
        "4waSgw==",
        ChecksumType::Composite,
    )
}

#[test]
fn crc64_nvme_full_object_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Crc64Nvme,
        "rosUhgp5mIg=",
        ChecksumType::FullObject,
    )
}

#[test]
fn sha1_composite_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Sha1,
        "98O8HYCOBHMq32eZZczDTKeuNEE=",
        ChecksumType::Composite,
    )
}

#[test]
fn sha256_composite_streams_and_reopens() -> TestResult {
    checksum_streams(
        ChecksumAlgorithm::Sha256,
        "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU=",
        ChecksumType::Composite,
    )
}
