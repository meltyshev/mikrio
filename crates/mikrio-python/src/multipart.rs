//! Detached multipart values and checked host conversions; storage stays in the facade.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString, PyTuple};

use crate::{native_error, storage_error, values};

pub(crate) fn count(value: &Bound<'_, PyAny>) -> PyResult<u32> {
    u32::try_from(values::integer(value)?).map_err(|_| {
        storage_error(
            value.py(),
            "InvalidArgument",
            "integer exceeds the supported range",
        )
    })
}

pub(crate) fn checksum_type(value: &Bound<'_, PyString>) -> PyResult<mikrio::ChecksumType> {
    match values::string(value)?.as_str() {
        "FULL_OBJECT" => Ok(mikrio::ChecksumType::FullObject),
        "COMPOSITE" => Ok(mikrio::ChecksumType::Composite),
        _ => Err(storage_error(
            value.py(),
            "InvalidArgument",
            "unsupported checksum type",
        )),
    }
}

pub(crate) fn copy_directive(value: &Bound<'_, PyString>) -> PyResult<mikrio::CopyDirective> {
    match values::string(value)?.as_str() {
        "COPY" => Ok(mikrio::CopyDirective::Copy),
        "REPLACE" => Ok(mikrio::CopyDirective::Replace),
        _ => Err(storage_error(
            value.py(),
            "InvalidArgument",
            "unsupported copy directive",
        )),
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct ObjectRef {
    pub(crate) value: mikrio::ObjectRef,
}

#[pymethods]
impl ObjectRef {
    #[new]
    #[pyo3(signature = (*, bucket, key))]
    fn new(bucket: &Bound<'_, PyString>, key: &Bound<'_, PyString>) -> PyResult<Self> {
        Ok(Self {
            value: mikrio::ObjectRef::new(&values::string(bucket)?, &values::string(key)?)
                .map_err(|error| native_error(bucket.py(), &error))?,
        })
    }

    #[getter]
    fn bucket(&self) -> &str {
        self.value.bucket()
    }

    #[getter]
    fn key(&self) -> &str {
        self.value.key()
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct CompletedPart {
    value: mikrio::CompletedPart,
}

#[pymethods]
impl CompletedPart {
    #[new]
    #[pyo3(signature = (*, part_number, etag, checksum_crc32=None, checksum_crc32c=None, checksum_sha1=None, checksum_sha256=None, checksum_crc64_nvme=None))]
    fn new(
        part_number: &Bound<'_, PyAny>,
        etag: &Bound<'_, PyString>,
        checksum_crc32: Option<&Bound<'_, PyString>>,
        checksum_crc32c: Option<&Bound<'_, PyString>>,
        checksum_sha1: Option<&Bound<'_, PyString>>,
        checksum_sha256: Option<&Bound<'_, PyString>>,
        checksum_crc64_nvme: Option<&Bound<'_, PyString>>,
    ) -> PyResult<Self> {
        let mut builder =
            mikrio::CompletedPart::builder(count(part_number)?, values::string(etag)?);

        if let Some(value) = checksum_crc32 {
            builder = builder.checksum_crc32(values::string(value)?);
        }

        if let Some(value) = checksum_crc32c {
            builder = builder.checksum_crc32c(values::string(value)?);
        }

        if let Some(value) = checksum_sha1 {
            builder = builder.checksum_sha1(values::string(value)?);
        }

        if let Some(value) = checksum_sha256 {
            builder = builder.checksum_sha256(values::string(value)?);
        }

        if let Some(value) = checksum_crc64_nvme {
            builder = builder.checksum_crc64_nvme(values::string(value)?);
        }

        Ok(Self {
            value: builder
                .build()
                .map_err(|error| native_error(part_number.py(), &error))?,
        })
    }

    #[getter]
    fn part_number(&self) -> u32 {
        self.value.part_number()
    }

    #[getter]
    fn etag(&self) -> &str {
        self.value.etag()
    }

    #[getter]
    fn checksum_crc32(&self) -> Option<&str> {
        self.value.checksum_crc32()
    }

    #[getter]
    fn checksum_crc32c(&self) -> Option<&str> {
        self.value.checksum_crc32c()
    }

    #[getter]
    fn checksum_sha1(&self) -> Option<&str> {
        self.value.checksum_sha1()
    }

    #[getter]
    fn checksum_sha256(&self) -> Option<&str> {
        self.value.checksum_sha256()
    }

    #[getter]
    fn checksum_crc64_nvme(&self) -> Option<&str> {
        self.value.checksum_crc64_nvme()
    }
}

pub(crate) fn parts(value: &Bound<'_, PyAny>) -> PyResult<Vec<mikrio::CompletedPart>> {
    let mut parts = Vec::new();

    for part in value.try_iter()? {
        if parts.len() == 10_000 {
            return Err(storage_error(
                value.py(),
                "InvalidArgument",
                "at most 10000 parts are accepted",
            ));
        }

        let part = part?;
        let part: PyRef<'_, CompletedPart> = part.extract()?;
        parts.push(part.value.clone());
    }

    Ok(parts)
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct CopyObjectOutput {
    etag: String,
    last_modified: Py<PyAny>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl CopyObjectOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::CopyObjectOutput) -> PyResult<Self> {
        Ok(Self {
            etag: output.etag().to_owned(),
            last_modified: values::datetime(py, output.last_modified())?,
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct CreateMultipartUploadOutput {
    upload_id: String,
    checksum_algorithm: String,
    checksum_type: String,
}

impl CreateMultipartUploadOutput {
    pub(crate) fn new(output: &mikrio::CreateMultipartUploadOutput) -> Self {
        Self {
            upload_id: output.upload_id().to_owned(),
            checksum_algorithm: output.checksum_algorithm().as_str().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
        }
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct UploadPartOutput {
    etag: String,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl UploadPartOutput {
    pub(crate) fn new(output: &mikrio::UploadPartOutput) -> Self {
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
pub(crate) struct UploadPartCopyOutput {
    etag: String,
    last_modified: Py<PyAny>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl UploadPartCopyOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::UploadPartCopyOutput) -> PyResult<Self> {
        Ok(Self {
            etag: output.etag().to_owned(),
            last_modified: values::datetime(py, output.last_modified())?,
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct CompleteMultipartUploadOutput {
    etag: String,
    checksum_type: String,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl CompleteMultipartUploadOutput {
    pub(crate) fn new(output: &mikrio::CompleteMultipartUploadOutput) -> Self {
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

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct AbortMultipartUploadOutput {}

impl AbortMultipartUploadOutput {
    pub(crate) fn new(_output: mikrio::AbortMultipartUploadOutput) -> Self {
        Self {}
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct PartSummary {
    part_number: u32,
    size: u64,
    etag: String,
    last_modified: Py<PyAny>,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl PartSummary {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::PartSummary) -> PyResult<Self> {
        Ok(Self {
            part_number: output.part_number(),
            size: output.size(),
            etag: output.etag().to_owned(),
            last_modified: values::datetime(py, output.last_modified())?,
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct MultipartUploadSummary {
    key: String,
    upload_id: String,
    initiated: Py<PyAny>,
    checksum_algorithm: String,
    checksum_type: String,
}

impl MultipartUploadSummary {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::MultipartUploadSummary) -> PyResult<Self> {
        Ok(Self {
            key: output.key().to_owned(),
            upload_id: output.upload_id().to_owned(),
            initiated: values::datetime(py, output.initiated())?,
            checksum_algorithm: output.checksum_algorithm().as_str().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ListPartsOutput {
    parts: Py<PyTuple>,
    is_truncated: bool,
    next_part_number_marker: Option<u32>,
    checksum_algorithm: String,
    checksum_type: String,
}

impl ListPartsOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::ListPartsOutput) -> PyResult<Self> {
        Ok(Self {
            parts: PyTuple::new(
                py,
                output
                    .parts()
                    .iter()
                    .map(|item| Py::new(py, PartSummary::new(py, item)?))
                    .collect::<PyResult<Vec<_>>>()?,
            )?
            .unbind(),
            is_truncated: output.is_truncated(),
            next_part_number_marker: output.next_part_number_marker(),
            checksum_algorithm: output.checksum_algorithm().as_str().to_owned(),
            checksum_type: output.checksum_type().as_str().to_owned(),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ListMultipartUploadsOutput {
    uploads: Py<PyTuple>,
    common_prefixes: Py<PyTuple>,
    is_truncated: bool,
    next_key_marker: Option<String>,
    next_upload_id_marker: Option<String>,
}

impl ListMultipartUploadsOutput {
    pub(crate) fn new(
        py: Python<'_>,
        output: &mikrio::ListMultipartUploadsOutput,
    ) -> PyResult<Self> {
        Ok(Self {
            uploads: PyTuple::new(
                py,
                output
                    .uploads()
                    .iter()
                    .map(|item| Py::new(py, MultipartUploadSummary::new(py, item)?))
                    .collect::<PyResult<Vec<_>>>()?,
            )?
            .unbind(),
            common_prefixes: PyTuple::new(
                py,
                output
                    .common_prefixes()
                    .iter()
                    .map(|prefix| {
                        Py::new(
                            py,
                            crate::listing::CommonPrefix {
                                prefix: prefix.prefix().to_owned(),
                            },
                        )
                    })
                    .collect::<PyResult<Vec<_>>>()?,
            )?
            .unbind(),
            is_truncated: output.is_truncated(),
            next_key_marker: output.next_key_marker().map(str::to_owned),
            next_upload_id_marker: output.next_upload_id_marker().map(str::to_owned),
        })
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<ObjectRef>()?;
    module.add_class::<CompletedPart>()?;
    module.add_class::<CopyObjectOutput>()?;
    module.add_class::<CreateMultipartUploadOutput>()?;
    module.add_class::<UploadPartOutput>()?;
    module.add_class::<UploadPartCopyOutput>()?;
    module.add_class::<CompleteMultipartUploadOutput>()?;
    module.add_class::<AbortMultipartUploadOutput>()?;
    module.add_class::<PartSummary>()?;
    module.add_class::<MultipartUploadSummary>()?;
    module.add_class::<ListPartsOutput>()?;
    module.add_class::<ListMultipartUploadsOutput>()?;

    Ok(())
}
