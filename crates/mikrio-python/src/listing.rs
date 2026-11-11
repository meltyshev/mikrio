//! Detached namespace values; no page retains a store or native snapshot.

use pyo3::prelude::*;
use pyo3::types::PyTuple;

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ObjectSummary {
    key: String,
    size: u64,
    etag: String,
    last_modified: Py<PyAny>,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct CommonPrefix {
    pub(crate) prefix: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ListObjectsV2Output {
    contents: Py<PyTuple>,
    common_prefixes: Py<PyTuple>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

impl ListObjectsV2Output {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::ListObjectsV2Output) -> PyResult<Self> {
        let contents = output
            .contents()
            .iter()
            .map(|object| {
                Py::new(
                    py,
                    ObjectSummary {
                        key: object.key().to_owned(),
                        size: object.size(),
                        etag: object.etag().to_owned(),
                        last_modified: crate::values::datetime(py, object.last_modified())?,
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        let prefixes = output
            .common_prefixes()
            .iter()
            .map(|prefix| {
                Py::new(
                    py,
                    CommonPrefix {
                        prefix: prefix.prefix().to_owned(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            contents: PyTuple::new(py, contents)?.unbind(),
            common_prefixes: PyTuple::new(py, prefixes)?.unbind(),
            is_truncated: output.is_truncated(),
            next_continuation_token: output.next_continuation_token().map(str::to_owned),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct BucketSummary {
    name: String,
    creation_date: Py<PyAny>,
    bucket_region: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ListBucketsOutput {
    buckets: Py<PyTuple>,
    continuation_token: Option<String>,
}

impl ListBucketsOutput {
    pub(crate) fn new(py: Python<'_>, output: &mikrio::ListBucketsOutput) -> PyResult<Self> {
        let buckets = output
            .buckets()
            .iter()
            .map(|bucket| {
                Py::new(
                    py,
                    BucketSummary {
                        name: bucket.name().to_owned(),
                        creation_date: crate::values::datetime(py, bucket.creation_date())?,
                        bucket_region: bucket.bucket_region().to_owned(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            buckets: PyTuple::new(py, buckets)?.unbind(),
            continuation_token: output.continuation_token().map(str::to_owned),
        })
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<BucketSummary>()?;
    module.add_class::<ListBucketsOutput>()?;
    module.add_class::<ObjectSummary>()?;
    module.add_class::<CommonPrefix>()?;
    module.add_class::<ListObjectsV2Output>()?;

    Ok(())
}
