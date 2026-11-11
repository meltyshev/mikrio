//! Immutable lifecycle values over the shared facade validation.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyInt, PyString, PyTuple};

use crate::{native_error, storage_error, values};

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct LifecycleFilter {
    inner: mikrio::LifecycleFilter,
}

#[pymethods]
impl LifecycleFilter {
    #[new]
    #[pyo3(signature = (*, prefix=None))]
    fn new(py: Python<'_>, prefix: Option<&Bound<'_, PyString>>) -> PyResult<Self> {
        Ok(Self {
            inner: mikrio::LifecycleFilter::new(prefix.map(values::string).transpose()?)
                .map_err(|error| native_error(py, &error))?,
        })
    }

    #[getter]
    fn prefix(&self) -> Option<&str> {
        self.inner.prefix()
    }
}

fn days(value: &Bound<'_, PyAny>) -> PyResult<u32> {
    if value.is_instance_of::<PyBool>() || !value.is_instance_of::<PyInt>() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "expected an integer, excluding bool",
        ));
    }

    value
        .extract::<u32>()
        .map_err(|_| storage_error(value.py(), "InvalidArgument", "invalid lifecycle days"))
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct LifecycleExpiration {
    inner: mikrio::LifecycleExpiration,
}

#[pymethods]
impl LifecycleExpiration {
    #[new]
    #[pyo3(signature = (*, days))]
    fn new(py: Python<'_>, days: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: mikrio::LifecycleExpiration::new(self::days(days)?)
                .map_err(|error| native_error(py, &error))?,
        })
    }

    #[getter]
    fn days(&self) -> u32 {
        self.inner.days()
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct AbortIncompleteMultipartUpload {
    inner: mikrio::AbortIncompleteMultipartUpload,
}

#[pymethods]
impl AbortIncompleteMultipartUpload {
    #[new]
    #[pyo3(signature = (*, days_after_initiation))]
    fn new(py: Python<'_>, days_after_initiation: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: mikrio::AbortIncompleteMultipartUpload::new(days(days_after_initiation)?)
                .map_err(|error| native_error(py, &error))?,
        })
    }

    #[getter]
    fn days_after_initiation(&self) -> u32 {
        self.inner.days_after_initiation()
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct LifecycleRule {
    inner: mikrio::LifecycleRule,
}

#[pymethods]
impl LifecycleRule {
    #[new]
    #[pyo3(signature = (*, status, filter, id=None, expiration=None, abort_incomplete_multipart_upload=None))]
    fn new(
        py: Python<'_>,
        status: &Bound<'_, PyString>,
        filter: &LifecycleFilter,
        id: Option<&Bound<'_, PyString>>,
        expiration: Option<&LifecycleExpiration>,
        abort_incomplete_multipart_upload: Option<&AbortIncompleteMultipartUpload>,
    ) -> PyResult<Self> {
        let status = match values::string(status)?.as_str() {
            "Enabled" => mikrio::LifecycleStatus::Enabled,
            "Disabled" => mikrio::LifecycleStatus::Disabled,
            _ => {
                return Err(storage_error(
                    py,
                    "InvalidArgument",
                    "invalid lifecycle status",
                ));
            }
        };

        let mut builder = mikrio::LifecycleRule::builder(status, filter.inner.clone());

        if let Some(id) = id {
            builder = builder.id(values::string(id)?);
        }

        if let Some(action) = expiration {
            builder = builder.expiration(action.inner);
        }

        if let Some(action) = abort_incomplete_multipart_upload {
            builder = builder.abort_incomplete_multipart_upload(action.inner);
        }

        Ok(Self {
            inner: builder.build().map_err(|error| native_error(py, &error))?,
        })
    }

    #[getter]
    fn id(&self) -> Option<&str> {
        self.inner.id()
    }

    #[getter]
    fn status(&self) -> &'static str {
        self.inner.status().as_str()
    }

    #[getter]
    fn filter(&self) -> LifecycleFilter {
        LifecycleFilter {
            inner: self.inner.filter().clone(),
        }
    }

    #[getter]
    fn expiration(&self) -> Option<LifecycleExpiration> {
        self.inner
            .expiration()
            .map(|inner| LifecycleExpiration { inner })
    }

    #[getter]
    fn abort_incomplete_multipart_upload(&self) -> Option<AbortIncompleteMultipartUpload> {
        self.inner
            .abort_incomplete_multipart_upload()
            .map(|inner| AbortIncompleteMultipartUpload { inner })
    }
}

pub(crate) fn rules(value: &Bound<'_, PyAny>) -> PyResult<Vec<mikrio::LifecycleRule>> {
    let mut rules = Vec::new();

    for item in value.try_iter()? {
        if rules.len() == 1000 {
            return Err(storage_error(
                value.py(),
                "InvalidArgument",
                "at most one thousand lifecycle rules are accepted",
            ));
        }

        let item = item?;
        let rule: PyRef<'_, LifecycleRule> = item.extract()?;
        rules.push(rule.inner.clone());
    }

    Ok(rules)
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketLifecycleConfigurationOutput {
    lifecycle_rules: Py<PyTuple>,
}

impl GetBucketLifecycleConfigurationOutput {
    pub(crate) fn new(
        py: Python<'_>,
        output: &mikrio::GetBucketLifecycleConfigurationOutput,
    ) -> PyResult<Self> {
        let rules = output
            .lifecycle_rules()
            .iter()
            .map(|rule| {
                Py::new(
                    py,
                    LifecycleRule {
                        inner: rule.clone(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            lifecycle_rules: PyTuple::new(py, rules)?.unbind(),
        })
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutBucketLifecycleConfigurationOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteBucketLifecycleOutput {}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<LifecycleFilter>()?;
    module.add_class::<LifecycleExpiration>()?;
    module.add_class::<AbortIncompleteMultipartUpload>()?;
    module.add_class::<LifecycleRule>()?;
    module.add_class::<PutBucketLifecycleConfigurationOutput>()?;
    module.add_class::<GetBucketLifecycleConfigurationOutput>()?;
    module.add_class::<DeleteBucketLifecycleOutput>()?;

    Ok(())
}
