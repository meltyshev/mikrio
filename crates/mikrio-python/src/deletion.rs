//! Immutable batch values and detached error progress.

use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};

use crate::{native_error, values};

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct ObjectIdentifier {
    pub(crate) inner: mikrio::ObjectIdentifier,
}

#[pymethods]
impl ObjectIdentifier {
    #[new]
    #[pyo3(signature = (*, key, etag=None))]
    fn new(
        py: Python<'_>,
        key: &Bound<'_, PyString>,
        etag: Option<&Bound<'_, PyString>>,
    ) -> PyResult<Self> {
        let mut builder = mikrio::ObjectIdentifier::builder(values::string(key)?);

        if let Some(etag) = etag {
            builder = builder.etag(values::string(etag)?);
        }

        Ok(Self {
            inner: builder.build().map_err(|error| native_error(py, &error))?,
        })
    }

    #[getter]
    fn key(&self) -> &str {
        self.inner.key()
    }

    #[getter]
    fn etag(&self) -> Option<&str> {
        self.inner.etag()
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct DeletedObject {
    key: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct DeleteError {
    code: String,
    message: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct DeleteObjectError {
    key: String,
    code: String,
    message: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct DeleteObjectsOutput {
    deleted: Py<PyTuple>,
    errors: Py<PyTuple>,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct DeleteObjectProgress {
    index: usize,
    key: String,
    mutation_outcome: String,
    error: Option<Py<DeleteError>>,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct DeleteObjectsProgress {
    items: Py<PyTuple>,
}

impl DeleteObjectsOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::DeleteObjectsOutput) -> PyResult<Self> {
        let deleted = output
            .deleted()
            .iter()
            .map(|item| {
                Py::new(
                    py,
                    DeletedObject {
                        key: item.key().into(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        let errors = output
            .errors()
            .iter()
            .map(|item| {
                Py::new(
                    py,
                    DeleteObjectError {
                        key: item.key().into(),
                        code: item.code().as_str().into(),
                        message: item.message().into(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            deleted: PyTuple::new(py, deleted)?.unbind(),
            errors: PyTuple::new(py, errors)?.unbind(),
        })
    }
}

impl DeleteObjectsProgress {
    pub(crate) fn new(py: Python<'_>, progress: &mikrio::DeleteObjectsProgress) -> PyResult<Self> {
        let items = progress
            .items()
            .iter()
            .map(|item| {
                let error = item
                    .error()
                    .map(|error| {
                        Py::new(
                            py,
                            DeleteError {
                                code: error.code().as_str().into(),
                                message: error.message().into(),
                            },
                        )
                    })
                    .transpose()?;

                Py::new(
                    py,
                    DeleteObjectProgress {
                        index: item.index(),
                        key: item.key().into(),
                        mutation_outcome: item.mutation_outcome().as_str().into(),
                        error,
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            items: PyTuple::new(py, items)?.unbind(),
        })
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<ObjectIdentifier>()?;
    module.add_class::<DeletedObject>()?;
    module.add_class::<DeleteError>()?;
    module.add_class::<DeleteObjectError>()?;
    module.add_class::<DeleteObjectsOutput>()?;
    module.add_class::<DeleteObjectProgress>()?;
    module.add_class::<DeleteObjectsProgress>()?;

    Ok(())
}
