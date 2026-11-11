//! Detached immutable tag results; the facade owns all validation.

use pyo3::prelude::*;
use pyo3::types::PyTuple;

use crate::outputs::Tag;

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetObjectTaggingOutput {
    tags: Py<PyTuple>,
}

impl GetObjectTaggingOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::GetObjectTaggingOutput) -> PyResult<Self> {
        let tags = output
            .tags()
            .iter()
            .map(|tag| {
                Py::new(
                    py,
                    Tag {
                        key: tag.key.clone(),
                        value: tag.value.clone(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            tags: PyTuple::new(py, tags)?.unbind(),
        })
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutObjectTaggingOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteObjectTaggingOutput {}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<GetObjectTaggingOutput>()?;
    module.add_class::<PutObjectTaggingOutput>()?;
    module.add_class::<DeleteObjectTaggingOutput>()?;

    Ok(())
}
