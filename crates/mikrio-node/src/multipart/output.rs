//! Detached native result values, projected only after confirmed facade success.

use napi::bindgen_prelude::BigInt;
use napi_derive::napi;

#[napi(object, object_from_js = false)]
pub struct NativeCopyObjectOutput {
    pub etag: String,
    pub last_modified: i64,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::CopyObjectOutput> for NativeCopyObjectOutput {
    fn from(output: &mikrio::CopyObjectOutput) -> Self {
        Self {
            etag: output.etag().to_owned(),
            last_modified: output.last_modified().as_unix_millis(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeCreateMultipartUploadOutput {
    pub upload_id: String,
    pub checksum_algorithm: String,
    pub checksum_type: String,
}

impl From<&mikrio::CreateMultipartUploadOutput> for NativeCreateMultipartUploadOutput {
    fn from(output: &mikrio::CreateMultipartUploadOutput) -> Self {
        Self {
            upload_id: output.upload_id().to_owned(),
            checksum_algorithm: output.checksum_algorithm().as_str().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeUploadPartOutput {
    pub etag: String,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::UploadPartOutput> for NativeUploadPartOutput {
    fn from(output: &mikrio::UploadPartOutput) -> Self {
        Self {
            etag: output.etag().to_owned(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeUploadPartCopyOutput {
    pub etag: String,
    pub last_modified: i64,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::UploadPartCopyOutput> for NativeUploadPartCopyOutput {
    fn from(output: &mikrio::UploadPartCopyOutput) -> Self {
        Self {
            etag: output.etag().to_owned(),
            last_modified: output.last_modified().as_unix_millis(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeCompleteMultipartUploadOutput {
    pub etag: String,
    pub checksum_type: String,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::CompleteMultipartUploadOutput> for NativeCompleteMultipartUploadOutput {
    fn from(output: &mikrio::CompleteMultipartUploadOutput) -> Self {
        Self {
            etag: output.etag().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativePart {
    pub part_number: u32,
    pub size: BigInt,
    pub etag: String,
    pub last_modified: i64,
    pub checksum_crc32: Option<String>,
    #[napi(js_name = "checksumCrc32c")]
    pub checksum_crc32c: Option<String>,
    pub checksum_sha1: Option<String>,
    pub checksum_sha256: Option<String>,
    pub checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::PartSummary> for NativePart {
    fn from(output: &mikrio::PartSummary) -> Self {
        Self {
            part_number: output.part_number(),
            size: output.size().into(),
            etag: output.etag().to_owned(),
            last_modified: output.last_modified().as_unix_millis(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeMultipartUpload {
    pub key: String,
    pub upload_id: String,
    pub initiated: i64,
    pub checksum_algorithm: String,
    pub checksum_type: String,
}

impl From<&mikrio::MultipartUploadSummary> for NativeMultipartUpload {
    fn from(output: &mikrio::MultipartUploadSummary) -> Self {
        Self {
            key: output.key().to_owned(),
            upload_id: output.upload_id().to_owned(),
            initiated: output.initiated().as_unix_millis(),
            checksum_algorithm: output.checksum_algorithm().as_str().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeListPartsOutput {
    pub parts: Vec<NativePart>,
    pub is_truncated: bool,
    pub next_part_number_marker: Option<u32>,
    pub checksum_algorithm: String,
    pub checksum_type: String,
}

impl From<&mikrio::ListPartsOutput> for NativeListPartsOutput {
    fn from(output: &mikrio::ListPartsOutput) -> Self {
        Self {
            parts: output.parts().iter().map(Into::into).collect(),
            is_truncated: output.is_truncated(),
            next_part_number_marker: output.next_part_number_marker(),
            checksum_algorithm: output.checksum_algorithm().as_str().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
        }
    }
}

#[napi(object, object_from_js = false)]
pub struct NativeListMultipartUploadsOutput {
    pub uploads: Vec<NativeMultipartUpload>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_key_marker: Option<String>,
    pub next_upload_id_marker: Option<String>,
}

impl From<&mikrio::ListMultipartUploadsOutput> for NativeListMultipartUploadsOutput {
    fn from(output: &mikrio::ListMultipartUploadsOutput) -> Self {
        Self {
            uploads: output.uploads().iter().map(Into::into).collect(),
            common_prefixes: output
                .common_prefixes()
                .iter()
                .map(|prefix| prefix.prefix().to_owned())
                .collect(),
            is_truncated: output.is_truncated(),
            next_key_marker: output.next_key_marker().map(str::to_owned),
            next_upload_id_marker: output.next_upload_id_marker().map(str::to_owned),
        }
    }
}
