//! Detached immutable operation outputs, without native storage owners.

use pyo3::prelude::*;

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct CreateBucketOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteBucketOutput {}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct HeadBucketOutput {
    pub(crate) bucket_region: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketLocationOutput {
    pub(crate) location_constraint: Option<String>,
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteObjectOutput {}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct Tag {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[pymethods]
impl Tag {
    #[new]
    #[pyo3(signature = (*, key, value))]
    fn new(
        key: &Bound<'_, pyo3::types::PyString>,
        value: &Bound<'_, pyo3::types::PyString>,
    ) -> PyResult<Self> {
        Ok(Self {
            key: crate::values::string(key)?,
            value: crate::values::string(value)?,
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct PutObjectOutput {
    etag: String,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl From<mikrio::PutObjectOutput> for PutObjectOutput {
    fn from(output: mikrio::PutObjectOutput) -> Self {
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

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetObjectOutput {
    body: Py<crate::body::ObjectBody>,
    content_length: u64,
    etag: String,
    last_modified: Py<PyAny>,
    metadata: Py<PyAny>,
    tag_count: u32,
    content_type: Option<String>,
    content_encoding: Option<String>,
    content_disposition: Option<String>,
    content_language: Option<String>,
    cache_control: Option<String>,
    expires: Option<Py<PyAny>>,
    content_range: Option<String>,
    parts_count: Option<u32>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
    checksum_type: Option<&'static str>,
}

impl GetObjectOutput {
    pub(crate) fn new(
        py: Python<'_>,
        output: &ReadDetails,
        body: Py<crate::body::ObjectBody>,
    ) -> PyResult<Self> {
        Ok(Self {
            body,
            content_length: output.content_length,
            etag: output.etag.clone(),
            last_modified: crate::values::datetime(py, output.last_modified)?,
            metadata: metadata(py, &output.metadata)?,
            tag_count: output.tag_count,
            content_type: output.content_type.clone(),
            content_encoding: output.content_encoding.clone(),
            content_disposition: output.content_disposition.clone(),
            content_language: output.content_language.clone(),
            cache_control: output.cache_control.clone(),
            expires: output
                .expires
                .map(|value| crate::values::datetime(py, value))
                .transpose()?,
            content_range: output.content_range.clone(),
            parts_count: output.parts_count,
            checksum_crc32: output.checksum_crc32.clone(),
            checksum_crc32c: output.checksum_crc32c.clone(),
            checksum_sha1: output.checksum_sha1.clone(),
            checksum_sha256: output.checksum_sha256.clone(),
            checksum_crc64_nvme: output.checksum_crc64_nvme.clone(),
            checksum_type: output.checksum_type,
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct HeadObjectOutput {
    content_length: u64,
    etag: String,
    last_modified: Py<PyAny>,
    metadata: Py<PyAny>,
    tag_count: u32,
    content_type: Option<String>,
    content_encoding: Option<String>,
    content_disposition: Option<String>,
    content_language: Option<String>,
    cache_control: Option<String>,
    expires: Option<Py<PyAny>>,
    content_range: Option<String>,
    parts_count: Option<u32>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
    checksum_type: Option<&'static str>,
}

impl HeadObjectOutput {
    pub(crate) fn new(py: Python<'_>, output: &ReadDetails) -> PyResult<Self> {
        Ok(Self {
            content_length: output.content_length,
            etag: output.etag.clone(),
            last_modified: crate::values::datetime(py, output.last_modified)?,
            metadata: metadata(py, &output.metadata)?,
            tag_count: output.tag_count,
            content_type: output.content_type.clone(),
            content_encoding: output.content_encoding.clone(),
            content_disposition: output.content_disposition.clone(),
            content_language: output.content_language.clone(),
            cache_control: output.cache_control.clone(),
            expires: output
                .expires
                .map(|value| crate::values::datetime(py, value))
                .transpose()?,
            content_range: output.content_range.clone(),
            parts_count: output.parts_count,
            checksum_crc32: output.checksum_crc32.clone(),
            checksum_crc32c: output.checksum_crc32c.clone(),
            checksum_sha1: output.checksum_sha1.clone(),
            checksum_sha256: output.checksum_sha256.clone(),
            checksum_crc64_nvme: output.checksum_crc64_nvme.clone(),
            checksum_type: output.checksum_type,
        })
    }
}

fn metadata(
    py: Python<'_>,
    entries: &std::collections::BTreeMap<String, String>,
) -> PyResult<Py<PyAny>> {
    let values = pyo3::types::PyDict::new(py);

    for (key, value) in entries {
        values.set_item(key, value)?;
    }

    Ok(py
        .import("types")?
        .getattr("MappingProxyType")?
        .call1((values,))?
        .unbind())
}

pub(crate) struct ReadDetails {
    content_length: u64,
    etag: String,
    last_modified: mikrio::Timestamp,
    metadata: std::collections::BTreeMap<String, String>,
    tag_count: u32,
    content_type: Option<String>,
    content_encoding: Option<String>,
    content_disposition: Option<String>,
    content_language: Option<String>,
    cache_control: Option<String>,
    expires: Option<mikrio::Timestamp>,
    content_range: Option<String>,
    parts_count: Option<u32>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
    checksum_type: Option<&'static str>,
}

impl From<&mikrio::GetObjectOutput> for ReadDetails {
    fn from(output: &mikrio::GetObjectOutput) -> Self {
        Self {
            content_length: output.content_length(),
            etag: output.etag().to_owned(),
            last_modified: output.last_modified(),
            metadata: output.metadata().clone(),
            tag_count: output.tag_count(),
            content_type: output.content_type().map(str::to_owned),
            content_encoding: output.content_encoding().map(str::to_owned),
            content_disposition: output.content_disposition().map(str::to_owned),
            content_language: output.content_language().map(str::to_owned),
            cache_control: output.cache_control().map(str::to_owned),
            expires: output.expires(),
            content_range: output.content_range().map(str::to_owned),
            parts_count: output.parts_count(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
            checksum_type: output.checksum_type().map(mikrio::ChecksumType::as_str),
        }
    }
}

impl From<&mikrio::HeadObjectOutput> for ReadDetails {
    fn from(output: &mikrio::HeadObjectOutput) -> Self {
        Self {
            content_length: output.content_length(),
            etag: output.etag().to_owned(),
            last_modified: output.last_modified(),
            metadata: output.metadata().clone(),
            tag_count: output.tag_count(),
            content_type: output.content_type().map(str::to_owned),
            content_encoding: output.content_encoding().map(str::to_owned),
            content_disposition: output.content_disposition().map(str::to_owned),
            content_language: output.content_language().map(str::to_owned),
            cache_control: output.cache_control().map(str::to_owned),
            expires: output.expires(),
            content_range: output.content_range().map(str::to_owned),
            parts_count: output.parts_count(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
            checksum_type: output.checksum_type().map(mikrio::ChecksumType::as_str),
        }
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CreateBucketOutput>()?;
    module.add_class::<DeleteBucketOutput>()?;
    module.add_class::<HeadBucketOutput>()?;
    module.add_class::<GetBucketLocationOutput>()?;
    module.add_class::<DeleteObjectOutput>()?;
    module.add_class::<PutObjectOutput>()?;
    module.add_class::<GetObjectOutput>()?;
    module.add_class::<HeadObjectOutput>()?;
    module.add_class::<Tag>()?;

    Ok(())
}
