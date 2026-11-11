//! Immutable host values; validation and persistence remain in the facade.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyInt, PyString, PyTuple};

use crate::{native_error, storage_error, values};

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct CorsRule {
    inner: mikrio::CorsRule,
}

#[pymethods]
impl CorsRule {
    #[new]
    #[pyo3(signature = (*, allowed_origins, allowed_methods, id=None, allowed_headers=None, expose_headers=None, max_age_seconds=None))]
    fn new(
        py: Python<'_>,
        allowed_origins: &Bound<'_, PyAny>,
        allowed_methods: &Bound<'_, PyAny>,
        id: Option<&Bound<'_, PyString>>,
        allowed_headers: Option<&Bound<'_, PyAny>>,
        expose_headers: Option<&Bound<'_, PyAny>>,
        max_age_seconds: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let mut builder =
            mikrio::CorsRule::builder(strings(allowed_origins)?, strings(allowed_methods)?);

        if let Some(id) = id {
            builder = builder.id(values::string(id)?);
        }

        if let Some(headers) = allowed_headers {
            builder = builder.allowed_headers(strings(headers)?);
        }

        if let Some(headers) = expose_headers {
            builder = builder.expose_headers(strings(headers)?);
        }

        if let Some(seconds) = max_age_seconds {
            if seconds.is_instance_of::<PyBool>() || !seconds.is_instance_of::<PyInt>() {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "expected an integer, excluding bool",
                ));
            }

            let seconds = seconds
                .extract::<i32>()
                .map_err(|_| storage_error(py, "InvalidArgument", "invalid CORS max age"))?;

            builder = builder.max_age_seconds(seconds);
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
    fn allowed_origins(&self, py: Python<'_>) -> PyResult<Py<PyTuple>> {
        Ok(PyTuple::new(py, self.inner.allowed_origins())?.unbind())
    }

    #[getter]
    fn allowed_methods(&self, py: Python<'_>) -> PyResult<Py<PyTuple>> {
        Ok(PyTuple::new(py, self.inner.allowed_methods())?.unbind())
    }

    #[getter]
    fn allowed_headers(&self, py: Python<'_>) -> PyResult<Option<Py<PyTuple>>> {
        self.inner
            .allowed_headers()
            .map(|headers| PyTuple::new(py, headers).map(Bound::unbind))
            .transpose()
    }

    #[getter]
    fn expose_headers(&self, py: Python<'_>) -> PyResult<Option<Py<PyTuple>>> {
        self.inner
            .expose_headers()
            .map(|headers| PyTuple::new(py, headers).map(Bound::unbind))
            .transpose()
    }

    #[getter]
    fn max_age_seconds(&self) -> Option<i32> {
        self.inner.max_age_seconds()
    }
}

fn strings(value: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    let mut result = Vec::new();
    let mut byte_count = 0_usize;

    for item in value.try_iter()? {
        let item = item?;
        let string = values::string(item.cast::<PyString>()?)?;
        byte_count = byte_count.saturating_add(string.len()).saturating_add(5);

        if byte_count > 64 * 1024 {
            return Err(storage_error(
                value.py(),
                "InvalidArgument",
                "oversized CORS list",
            ));
        }

        result.push(string);
    }

    Ok(result)
}

pub(crate) fn rules(value: &Bound<'_, PyAny>) -> PyResult<Vec<mikrio::CorsRule>> {
    let mut rules = Vec::new();

    for item in value.try_iter()? {
        if rules.len() == 100 {
            return Err(storage_error(
                value.py(),
                "InvalidArgument",
                "at most one hundred CORS rules are accepted",
            ));
        }

        let item = item?;
        let rule: PyRef<'_, CorsRule> = item.extract()?;
        rules.push(rule.inner.clone());
    }

    Ok(rules)
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketCorsOutput {
    cors_rules: Py<PyTuple>,
}

impl GetBucketCorsOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::GetBucketCorsOutput) -> PyResult<Self> {
        let rules = output
            .cors_rules()
            .iter()
            .map(|rule| {
                Py::new(
                    py,
                    CorsRule {
                        inner: rule.clone(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            cors_rules: PyTuple::new(py, rules)?.unbind(),
        })
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutBucketCorsOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteBucketCorsOutput {}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CorsRule>()?;
    module.add_class::<PutBucketCorsOutput>()?;
    module.add_class::<GetBucketCorsOutput>()?;
    module.add_class::<DeleteBucketCorsOutput>()?;

    Ok(())
}
