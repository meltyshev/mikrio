//! Detached administration projections; Python never receives SQL or runtime owners.

use pyo3::prelude::*;
use pyo3::types::PyTuple;

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct OwnerInfo {
    account_id: String,
    canonical_id: String,
}

impl OwnerInfo {
    pub(crate) fn new(value: &mikrio::OwnerInfo) -> Self {
        Self {
            account_id: value.account_id().to_owned(),
            canonical_id: value.canonical_id().to_owned(),
        }
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct PrincipalInfo {
    principal_id: String,
    name: String,
    is_enabled: bool,
    created_at: Py<PyAny>,
}

impl PrincipalInfo {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::PrincipalInfo) -> PyResult<Self> {
        Ok(Self {
            principal_id: value.principal_id().to_owned(),
            name: value.name().to_owned(),
            is_enabled: value.is_enabled(),
            created_at: crate::values::datetime(py, value.created_at())?,
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct AccessKeyInfo {
    access_key_id: String,
    principal_id: String,
    created_at: Py<PyAny>,
}

impl AccessKeyInfo {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::AccessKeyInfo) -> PyResult<Self> {
        Ok(Self {
            access_key_id: value.access_key_id().to_owned(),
            principal_id: value.principal_id().to_owned(),
            created_at: crate::values::datetime(py, value.created_at())?,
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct CreateAccessKeyOutput {
    access_key: Py<AccessKeyInfo>,
    secret_access_key: String,
}

impl CreateAccessKeyOutput {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::CreateAccessKeyOutput) -> PyResult<Self> {
        Ok(Self {
            access_key: Py::new(py, AccessKeyInfo::new(py, value.access_key())?)?,
            secret_access_key: value.secret_access_key().to_owned(),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ListPrincipalsOutput {
    principals: Py<PyTuple>,
    next_continuation_token: Option<String>,
}

impl ListPrincipalsOutput {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::ListPrincipalsOutput) -> PyResult<Self> {
        Ok(Self {
            principals: PyTuple::new(
                py,
                value
                    .principals()
                    .iter()
                    .map(|item| Py::new(py, PrincipalInfo::new(py, item)?))
                    .collect::<PyResult<Vec<_>>>()?,
            )?
            .unbind(),
            next_continuation_token: value.next_continuation_token().map(str::to_owned),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct ListAccessKeysOutput {
    access_keys: Py<PyTuple>,
    next_continuation_token: Option<String>,
}

impl ListAccessKeysOutput {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::ListAccessKeysOutput) -> PyResult<Self> {
        Ok(Self {
            access_keys: PyTuple::new(
                py,
                value
                    .access_keys()
                    .iter()
                    .map(|item| Py::new(py, AccessKeyInfo::new(py, item)?))
                    .collect::<PyResult<Vec<_>>>()?,
            )?
            .unbind(),
            next_continuation_token: value.next_continuation_token().map(str::to_owned),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetPrincipalPolicyOutput {
    document: String,
}

impl GetPrincipalPolicyOutput {
    pub(crate) fn new(value: &mikrio::GetPrincipalPolicyOutput) -> Self {
        Self {
            document: value.document().to_owned(),
        }
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutPrincipalPolicyOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeletePrincipalPolicyOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteAccessKeyOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeletePrincipalOutput {}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<OwnerInfo>()?;
    module.add_class::<PrincipalInfo>()?;
    module.add_class::<AccessKeyInfo>()?;
    module.add_class::<CreateAccessKeyOutput>()?;
    module.add_class::<ListPrincipalsOutput>()?;
    module.add_class::<ListAccessKeysOutput>()?;
    module.add_class::<GetPrincipalPolicyOutput>()?;
    module.add_class::<PutPrincipalPolicyOutput>()?;
    module.add_class::<DeletePrincipalPolicyOutput>()?;
    module.add_class::<DeleteAccessKeyOutput>()?;
    module.add_class::<DeletePrincipalOutput>()?;

    Ok(())
}
