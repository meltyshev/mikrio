//! Immutable Attributes projections; no storage or protocol implementation.

use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};

pub(crate) fn selectors(value: &Bound<'_, PyAny>) -> PyResult<Vec<mikrio::ObjectAttribute>> {
    let mut selected = Vec::with_capacity(5);

    for item in value.try_iter()? {
        if selected.len() == 5 {
            return Err(crate::storage_error(
                value.py(),
                "InvalidArgument",
                "at most five attribute selectors are allowed",
            ));
        }

        let item = item?;

        selected.push(
            mikrio::ObjectAttribute::parse(&crate::values::string(item.cast::<PyString>()?)?)
                .map_err(|error| crate::native_error(value.py(), &error))?,
        );
    }

    Ok(selected)
}

#[pyclass(frozen, module = "mikrio", get_all)]
#[expect(
    clippy::struct_field_names,
    reason = "Python field names implement the accepted Checksum schema"
)]
pub(crate) struct Checksum {
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
    checksum_type: Option<&'static str>,
}

impl From<&mikrio::Checksum> for Checksum {
    fn from(output: &mikrio::Checksum) -> Self {
        Self {
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
            checksum_type: output.checksum_type().map(mikrio::ChecksumType::as_str),
        }
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ObjectPart {
    part_number: u32,
    size: u64,
    checksum_crc32: Option<String>,
    checksum_crc32c: Option<String>,
    checksum_sha1: Option<String>,
    checksum_sha256: Option<String>,
    checksum_crc64_nvme: Option<String>,
}

impl From<&mikrio::ObjectPart> for ObjectPart {
    fn from(output: &mikrio::ObjectPart) -> Self {
        Self {
            part_number: output.part_number(),
            size: output.size(),
            checksum_crc32: output.checksum_crc32().map(str::to_owned),
            checksum_crc32c: output.checksum_crc32c().map(str::to_owned),
            checksum_sha1: output.checksum_sha1().map(str::to_owned),
            checksum_sha256: output.checksum_sha256().map(str::to_owned),
            checksum_crc64_nvme: output.checksum_crc64_nvme().map(str::to_owned),
        }
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ObjectParts {
    total_parts_count: u32,
    max_parts: u32,
    part_number_marker: u32,
    parts: Py<PyTuple>,
    is_truncated: bool,
    next_part_number_marker: Option<u32>,
}

impl ObjectParts {
    fn new(py: Python<'_>, output: &mikrio::ObjectParts) -> PyResult<Self> {
        let parts = output
            .parts()
            .iter()
            .map(|part| Py::new(py, ObjectPart::from(part)))
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            total_parts_count: output.total_parts_count(),
            max_parts: output.max_parts(),
            part_number_marker: output.part_number_marker(),
            parts: PyTuple::new(py, parts)?.unbind(),
            is_truncated: output.is_truncated(),
            next_part_number_marker: output.next_part_number_marker(),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetObjectAttributesOutput {
    object_size: Option<u64>,
    etag: Option<String>,
    checksum: Option<Py<Checksum>>,
    storage_class: Option<String>,
    object_parts: Option<Py<ObjectParts>>,
    last_modified: Py<PyAny>,
}

impl GetObjectAttributesOutput {
    pub(crate) fn new(
        py: Python<'_>,
        output: &mikrio::GetObjectAttributesOutput,
    ) -> PyResult<Self> {
        Ok(Self {
            object_size: output.object_size(),
            etag: output.etag().map(str::to_owned),
            checksum: output
                .checksum()
                .map(|value| Py::new(py, Checksum::from(value)))
                .transpose()?,
            storage_class: output.storage_class().map(str::to_owned),
            object_parts: output
                .object_parts()
                .map(|value| Py::new(py, ObjectParts::new(py, value)?))
                .transpose()?,
            last_modified: crate::values::datetime(py, output.last_modified())?,
        })
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Checksum>()?;
    module.add_class::<ObjectPart>()?;
    module.add_class::<ObjectParts>()?;
    module.add_class::<GetObjectAttributesOutput>()?;

    Ok(())
}
