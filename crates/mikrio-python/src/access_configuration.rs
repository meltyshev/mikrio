//! Frozen configuration and ACL values with no connection or SQL ownership.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyTuple};

/// A deliberate complete replacement; every flag is a required Python boolean.
#[pyclass(frozen, module = "mikrio", get_all)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent S3 configuration flags"
)]
pub(crate) struct PublicAccessBlockConfiguration {
    block_public_acls: bool,
    ignore_public_acls: bool,
    block_public_policy: bool,
    restrict_public_buckets: bool,
}

#[pymethods]
impl PublicAccessBlockConfiguration {
    #[new]
    #[pyo3(signature = (*, block_public_acls, ignore_public_acls, block_public_policy, restrict_public_buckets))]
    fn new(
        block_public_acls: &Bound<'_, PyBool>,
        ignore_public_acls: &Bound<'_, PyBool>,
        block_public_policy: &Bound<'_, PyBool>,
        restrict_public_buckets: &Bound<'_, PyBool>,
    ) -> Self {
        Self {
            block_public_acls: block_public_acls.is_true(),
            ignore_public_acls: ignore_public_acls.is_true(),
            block_public_policy: block_public_policy.is_true(),
            restrict_public_buckets: restrict_public_buckets.is_true(),
        }
    }
}

impl PublicAccessBlockConfiguration {
    pub(crate) fn to_native(&self) -> mikrio::PublicAccessBlockConfiguration {
        mikrio::PublicAccessBlockConfiguration {
            block_public_acls: self.block_public_acls,
            ignore_public_acls: self.ignore_public_acls,
            block_public_policy: self.block_public_policy,
            restrict_public_buckets: self.restrict_public_buckets,
        }
    }

    fn from_native(value: mikrio::PublicAccessBlockConfiguration) -> Self {
        Self {
            block_public_acls: value.block_public_acls,
            ignore_public_acls: value.ignore_public_acls,
            block_public_policy: value.block_public_policy,
            restrict_public_buckets: value.restrict_public_buckets,
        }
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketPolicyOutput {
    pub(crate) document: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketPolicyStatusOutput {
    pub(crate) is_public: bool,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetPublicAccessBlockOutput {
    configuration: Py<PublicAccessBlockConfiguration>,
}

impl GetPublicAccessBlockOutput {
    pub(crate) fn new(py: Python<'_>, value: mikrio::GetPublicAccessBlockOutput) -> PyResult<Self> {
        Ok(Self {
            configuration: Py::new(
                py,
                PublicAccessBlockConfiguration::from_native(value.configuration()),
            )?,
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketOwnershipControlsOutput {
    pub(crate) object_ownership: &'static str,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct AclOwner {
    canonical_id: String,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct AclGrant {
    grantee_canonical_id: String,
    permission: &'static str,
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetBucketAclOutput {
    owner: Py<AclOwner>,
    grants: Py<PyTuple>,
}

impl GetBucketAclOutput {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::GetBucketAclOutput) -> PyResult<Self> {
        let owner = Py::new(
            py,
            AclOwner {
                canonical_id: value.owner().canonical_id().into(),
            },
        )?;

        let grants = value
            .grants()
            .iter()
            .map(|grant| {
                Py::new(
                    py,
                    AclGrant {
                        grantee_canonical_id: grant.grantee_canonical_id().into(),
                        permission: match grant.permission() {
                            mikrio::AclPermission::FullControl => "FULL_CONTROL",
                        },
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            owner,
            grants: PyTuple::new(py, grants)?.unbind(),
        })
    }
}

#[pyclass(frozen, module = "mikrio", get_all)]
pub(crate) struct GetObjectAclOutput {
    owner: Py<AclOwner>,
    grants: Py<PyTuple>,
}

impl GetObjectAclOutput {
    pub(crate) fn new(py: Python<'_>, value: &mikrio::GetObjectAclOutput) -> PyResult<Self> {
        let owner = Py::new(
            py,
            AclOwner {
                canonical_id: value.owner().canonical_id().into(),
            },
        )?;

        let grants = value
            .grants()
            .iter()
            .map(|grant| {
                Py::new(
                    py,
                    AclGrant {
                        grantee_canonical_id: grant.grantee_canonical_id().into(),
                        permission: match grant.permission() {
                            mikrio::AclPermission::FullControl => "FULL_CONTROL",
                        },
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;

        Ok(Self {
            owner,
            grants: PyTuple::new(py, grants)?.unbind(),
        })
    }
}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutBucketPolicyOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeleteBucketPolicyOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutPublicAccessBlockOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct DeletePublicAccessBlockOutput {}

#[pyclass(frozen, module = "mikrio")]
pub(crate) struct PutBucketOwnershipControlsOutput {}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PutBucketPolicyOutput>()?;
    module.add_class::<GetBucketPolicyOutput>()?;
    module.add_class::<DeleteBucketPolicyOutput>()?;
    module.add_class::<GetBucketPolicyStatusOutput>()?;
    module.add_class::<PutPublicAccessBlockOutput>()?;
    module.add_class::<GetPublicAccessBlockOutput>()?;
    module.add_class::<DeletePublicAccessBlockOutput>()?;
    module.add_class::<PutBucketOwnershipControlsOutput>()?;
    module.add_class::<GetBucketOwnershipControlsOutput>()?;
    module.add_class::<GetBucketAclOutput>()?;
    module.add_class::<GetObjectAclOutput>()?;
    module.add_class::<PublicAccessBlockConfiguration>()?;
    module.add_class::<AclOwner>()?;
    module.add_class::<AclGrant>()?;

    Ok(())
}
