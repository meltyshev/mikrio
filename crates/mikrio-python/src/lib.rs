//! Synchronous Python value translation over the shared Rust facade.

use std::collections::HashSet;
use std::error::Error;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use mikrio::{OpenOptions, Store as NativeStore};
use mikrio_embed::{CloseGate, HostError, HostRuntime, Pending, ProcessBound, check_process};
use pyo3::create_exception;
use pyo3::exceptions::{PyBaseException, PyException, PyOSError, PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyMapping, PyString};

mod access_configuration;
mod admin;
mod body;
mod cors;
mod deletion;
mod diagnostics;
mod lifecycle;
mod listener;
mod listing;
mod multipart;
mod object_attributes;
mod object_tagging;
mod outputs;
mod upload;
mod values;

#[cfg(test)]
mod tests;

create_exception!(mikrio, StorageError, PyException);

/// Detached store identity and configuration; readable after the connection closes.
#[pyclass(frozen, module = "mikrio", get_all)]
#[derive(Clone)]
struct StoreInfo {
    store_id: String,
    format_version: u32,
    directory: String,
    region: String,
}

struct Owner {
    store: NativeStore,
    runtime: Arc<HostRuntime>,
    close: CloseGate,
}

/// Synchronous trusted access to a local store through one owned connection.
///
/// Use a context manager or close explicitly to settle native work. Blocking waits
/// release the interpreter and preserve Python interruptions; interruption does not
/// establish that a mutation rolled back. Inherited handles cannot be used after fork.
#[pyclass(frozen, module = "mikrio")]
struct Store {
    owner: ProcessBound<Owner>,
    info: StoreInfo,
}

#[pymethods]
impl Store {
    /// Open or initialize a managed UTF-8 directory without resetting existing data.
    ///
    /// An omitted region reuses the persisted region, or defaults to us-east-1 for
    /// a new store. Invalid directories and region mismatches raise `StorageError`.
    /// `create_if_missing=False` requires recognized storage; recovery and
    /// maintenance remain enabled. The default permits initialization.
    #[staticmethod]
    #[pyo3(signature = (*, directory, region=None, create_if_missing=true))]
    fn open(
        py: Python<'_>,
        directory: &Bound<'_, PyAny>,
        region: Option<&Bound<'_, PyString>>,
        create_if_missing: bool,
    ) -> PyResult<Self> {
        check_process().map_err(|error| host_error(&error))?;
        let directory = py.import("os")?.getattr("fspath")?.call1((directory,))?;

        let directory = directory
            .cast::<PyString>()
            .map_err(|_| PyTypeError::new_err("directory must be str or os.PathLike[str]"))?;

        let directory = directory
            .to_str()
            .map_err(|_| {
                storage_error(
                    py,
                    "InvalidArgument",
                    "directory must be representable as UTF-8",
                )
            })?
            .to_owned();

        let mut options = OpenOptions::new(directory).create_if_missing(create_if_missing);

        if let Some(region) = region {
            let region = region.to_str().map_err(|_| {
                storage_error(
                    py,
                    "InvalidArgument",
                    "region must be representable as UTF-8",
                )
            })?;

            options = options.region(region);
        }

        let runtime = py
            .detach(HostRuntime::shared)
            .map_err(|error| host_error(&error))?;

        let pending = runtime
            .submit(mikrio::embedding::open(options))
            .map_err(|error| host_error(&error))?;

        let (store, diagnostics) = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        let facts = store.info();

        let info = StoreInfo {
            store_id: facts.store_id().to_owned(),
            format_version: facts.format_version(),
            directory: facts
                .directory()
                .to_str()
                .ok_or_else(|| {
                    storage_error(
                        py,
                        "InvalidArgument",
                        "directory must be representable as UTF-8",
                    )
                })?
                .to_owned(),
            region: facts.region().to_owned(),
        };

        diagnostics::start(py, &diagnostics)?;

        Ok(Self {
            owner: ProcessBound::new(Owner {
                store,
                runtime,
                close: CloseGate::default(),
            }),
            info,
        })
    }

    #[getter]
    fn info(&self) -> PyResult<StoreInfo> {
        self.owner.get().map_err(|error| host_error(&error))?;
        Ok(self.info.clone())
    }

    /// Start an owned HTTP listener and return once its endpoint is ready.
    ///
    /// Only one listener may be active on this connection. Closing the returned
    /// server leaves direct store operations available.
    #[pyo3(signature = (*, host=None, port=None, public_endpoint=None, credentials=None))]
    fn listen(
        &self,
        py: Python<'_>,
        host: Option<&Bound<'_, PyString>>,
        port: Option<&Bound<'_, PyAny>>,
        public_endpoint: Option<&Bound<'_, PyString>>,
        credentials: Option<&listener::Credentials>,
    ) -> PyResult<listener::Server> {
        listener::listen(self, py, host, port, public_endpoint, credentials)
    }

    /// Settle this connection's work and close its listeners and bodies.
    ///
    /// Repeated calls join shutdown. A `ShutdownFailed` error retains ownership for
    /// another close attempt; independent connections remain usable.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let store = owner.store.clone();

        let pending = owner
            .close
            .begin(&owner.runtime, async move { store.close().await })
            .map_err(|error| host_error(&error))?;

        wait_close(py, &pending)
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
        slf.owner.get().map_err(|error| host_error(&error))?;
        Ok(slf)
    }

    fn __exit__(
        &self,
        py: Python<'_>,
        _exception_type: &Bound<'_, PyAny>,
        exception: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        context_exit(py, exception, self.close(py))
    }

    fn get_owner_info(&self, py: Python<'_>) -> PyResult<admin::OwnerInfo> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_owner_info().await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(admin::OwnerInfo::new(&output))
    }

    #[pyo3(signature = (*, name))]
    fn create_principal(
        &self,
        py: Python<'_>,
        name: &Bound<'_, PyString>,
    ) -> PyResult<admin::PrincipalInfo> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::CreatePrincipalInput::builder(
            values::string(name).map_err(|error| before_mutation(py, error))?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.create_principal(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        admin::PrincipalInfo::new(py, &output)
    }

    #[pyo3(signature = (*, principal_id))]
    fn get_principal(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
    ) -> PyResult<admin::PrincipalInfo> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let builder = mikrio::GetPrincipalInput::builder(values::string(principal_id)?);
        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_principal(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        admin::PrincipalInfo::new(py, &output)
    }

    #[pyo3(signature = (*, principal_id, is_enabled))]
    fn set_principal_enabled(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
        is_enabled: &Bound<'_, PyBool>,
    ) -> PyResult<admin::PrincipalInfo> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::SetPrincipalEnabledInput::builder(
            values::string(principal_id).map_err(|error| before_mutation(py, error))?,
            is_enabled.extract::<bool>()?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.set_principal_enabled(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        admin::PrincipalInfo::new(py, &output)
    }

    #[pyo3(signature = (*, principal_id))]
    fn delete_principal(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
    ) -> PyResult<admin::DeletePrincipalOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::DeletePrincipalInput::builder(
            values::string(principal_id).map_err(|error| before_mutation(py, error))?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_principal(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(admin::DeletePrincipalOutput {})
    }

    #[pyo3(signature = (*, principal_id, document))]
    fn put_principal_policy(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
        document: &Bound<'_, PyString>,
    ) -> PyResult<admin::PutPrincipalPolicyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::PutPrincipalPolicyInput::builder(
            values::string(principal_id).map_err(|error| before_mutation(py, error))?,
            values::string(document).map_err(|error| before_mutation(py, error))?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_principal_policy(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(admin::PutPrincipalPolicyOutput {})
    }

    #[pyo3(signature = (*, principal_id))]
    fn get_principal_policy(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
    ) -> PyResult<admin::GetPrincipalPolicyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let builder = mikrio::GetPrincipalPolicyInput::builder(values::string(principal_id)?);
        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_principal_policy(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(admin::GetPrincipalPolicyOutput::new(&output))
    }

    #[pyo3(signature = (*, principal_id))]
    fn delete_principal_policy(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
    ) -> PyResult<admin::DeletePrincipalPolicyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::DeletePrincipalPolicyInput::builder(
            values::string(principal_id).map_err(|error| before_mutation(py, error))?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_principal_policy(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(admin::DeletePrincipalPolicyOutput {})
    }

    #[pyo3(signature = (*, principal_id))]
    fn create_access_key(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
    ) -> PyResult<admin::CreateAccessKeyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::CreateAccessKeyInput::builder(
            values::string(principal_id).map_err(|error| before_mutation(py, error))?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.create_access_key(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        admin::CreateAccessKeyOutput::new(py, &output)
    }

    #[pyo3(signature = (*, principal_id, access_key_id))]
    fn delete_access_key(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
        access_key_id: &Bound<'_, PyString>,
    ) -> PyResult<admin::DeleteAccessKeyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let builder = mikrio::DeleteAccessKeyInput::builder(
            values::string(principal_id).map_err(|error| before_mutation(py, error))?,
            values::string(access_key_id).map_err(|error| before_mutation(py, error))?,
        );

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_access_key(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(admin::DeleteAccessKeyOutput {})
    }

    #[pyo3(signature = (*, max_results=None, continuation_token=None))]
    fn list_principals(
        &self,
        py: Python<'_>,
        max_results: Option<&Bound<'_, PyAny>>,
        continuation_token: Option<&Bound<'_, PyString>>,
    ) -> PyResult<admin::ListPrincipalsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let mut builder = mikrio::ListPrincipalsInput::builder();

        if let Some(value) = max_results {
            let value = u32::try_from(values::integer(value)?).map_err(|_| {
                storage_error(
                    py,
                    "InvalidArgument",
                    "page size is outside the supported range",
                )
            })?;

            builder = builder.max_results(value);
        }

        if let Some(value) = continuation_token {
            builder = builder.continuation_token(values::string(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.list_principals(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        admin::ListPrincipalsOutput::new(py, &output)
    }

    #[pyo3(signature = (*, principal_id, max_results=None, continuation_token=None))]
    fn list_access_keys(
        &self,
        py: Python<'_>,
        principal_id: &Bound<'_, PyString>,
        max_results: Option<&Bound<'_, PyAny>>,
        continuation_token: Option<&Bound<'_, PyString>>,
    ) -> PyResult<admin::ListAccessKeysOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let mut builder = mikrio::ListAccessKeysInput::builder(values::string(principal_id)?);

        if let Some(value) = max_results {
            let value = u32::try_from(values::integer(value)?).map_err(|_| {
                storage_error(
                    py,
                    "InvalidArgument",
                    "page size is outside the supported range",
                )
            })?;

            builder = builder.max_results(value);
        }

        if let Some(value) = continuation_token {
            builder = builder.continuation_token(values::string(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.list_access_keys(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        admin::ListAccessKeysOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, object_attributes, expected_bucket_owner=None, max_parts=None, part_number_marker=None))]
    fn get_object_attributes(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        object_attributes: &Bound<'_, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        max_parts: Option<&Bound<'_, PyAny>>,
        part_number_marker: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<object_attributes::GetObjectAttributesOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetObjectAttributesInput::builder(
            values::string(bucket)?,
            values::string(key)?,
            object_attributes::selectors(object_attributes)?,
        );

        if let Some(value) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(value)?);
        }

        if let Some(value) = max_parts {
            builder = builder.max_parts(multipart::count(value)?);
        }

        if let Some(value) = part_number_marker {
            builder = builder.part_number_marker(multipart::count(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_object_attributes(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        object_attributes::GetObjectAttributesOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None))]
    fn get_object_tagging(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<object_tagging::GetObjectTaggingOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder =
            mikrio::GetObjectTaggingInput::builder(values::string(bucket)?, values::string(key)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_object_tagging(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        object_tagging::GetObjectTaggingOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, tags, expected_bucket_owner=None))]
    fn put_object_tagging(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        tags: &Bound<'_, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<object_tagging::PutObjectTaggingOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::PutObjectTaggingInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                values::tags(tags)?,
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_object_tagging(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(object_tagging::PutObjectTaggingOutput {})
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None))]
    fn delete_object_tagging(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<object_tagging::DeleteObjectTaggingOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::DeleteObjectTaggingInput::builder(
                values::string(bucket)?,
                values::string(key)?,
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_object_tagging(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(object_tagging::DeleteObjectTaggingOutput {})
    }

    #[pyo3(signature = (*, bucket, cors_rules, expected_bucket_owner=None))]
    fn put_bucket_cors(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        cors_rules: &Bound<'_, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<cors::PutBucketCorsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::PutBucketCorsInput::builder(
                values::string(bucket)?,
                cors::rules(cors_rules)?,
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_bucket_cors(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(cors::PutBucketCorsOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_cors(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<cors::GetBucketCorsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetBucketCorsInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_cors(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        cors::GetBucketCorsOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn delete_bucket_cors(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<cors::DeleteBucketCorsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::DeleteBucketCorsInput::builder(values::string(bucket)?);

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_bucket_cors(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(cors::DeleteBucketCorsOutput {})
    }

    #[pyo3(signature = (*, bucket, lifecycle_rules, expected_bucket_owner=None))]
    fn put_bucket_lifecycle_configuration(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        lifecycle_rules: &Bound<'_, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<lifecycle::PutBucketLifecycleConfigurationOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::PutBucketLifecycleConfigurationInput::builder(
                values::string(bucket)?,
                lifecycle::rules(lifecycle_rules)?,
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_bucket_lifecycle_configuration(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(lifecycle::PutBucketLifecycleConfigurationOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_lifecycle_configuration(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<lifecycle::GetBucketLifecycleConfigurationOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder =
            mikrio::GetBucketLifecycleConfigurationInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_lifecycle_configuration(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        lifecycle::GetBucketLifecycleConfigurationOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn delete_bucket_lifecycle(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<lifecycle::DeleteBucketLifecycleOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::DeleteBucketLifecycleInput::builder(values::string(bucket)?);

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_bucket_lifecycle(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(lifecycle::DeleteBucketLifecycleOutput {})
    }

    #[pyo3(signature = (*, bucket, document, expected_bucket_owner=None))]
    fn put_bucket_policy(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        document: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::PutBucketPolicyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::PutBucketPolicyInput::builder(
                values::string(bucket)?,
                values::string(document)?,
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_bucket_policy(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::PutBucketPolicyOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_policy(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::GetBucketPolicyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetBucketPolicyInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_policy(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::GetBucketPolicyOutput {
            document: output.document().into(),
        })
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn delete_bucket_policy(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::DeleteBucketPolicyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::DeleteBucketPolicyInput::builder(values::string(bucket)?);

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_bucket_policy(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::DeleteBucketPolicyOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_policy_status(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::GetBucketPolicyStatusOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetBucketPolicyStatusInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_policy_status(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::GetBucketPolicyStatusOutput {
            is_public: output.is_public(),
        })
    }

    #[pyo3(signature = (*, bucket, configuration, expected_bucket_owner=None))]
    fn put_public_access_block(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        configuration: &access_configuration::PublicAccessBlockConfiguration,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::PutPublicAccessBlockOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::PutPublicAccessBlockInput::builder(
                values::string(bucket)?,
                configuration.to_native(),
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_public_access_block(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::PutPublicAccessBlockOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_public_access_block(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::GetPublicAccessBlockOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetPublicAccessBlockInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_public_access_block(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        access_configuration::GetPublicAccessBlockOutput::new(py, output)
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn delete_public_access_block(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::DeletePublicAccessBlockOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder =
                mikrio::DeletePublicAccessBlockInput::builder(values::string(bucket)?);

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_public_access_block(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::DeletePublicAccessBlockOutput {})
    }

    #[pyo3(signature = (*, bucket, object_ownership, expected_bucket_owner=None))]
    fn put_bucket_ownership_controls(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        object_ownership: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::PutBucketOwnershipControlsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let ownership = match values::string(object_ownership)?.as_str() {
                "BucketOwnerEnforced" => mikrio::ObjectOwnership::BucketOwnerEnforced,
                _ => {
                    return Err(storage_error(
                        py,
                        "InvalidArgument",
                        "unsupported ownership mode",
                    ));
                }
            };

            let mut builder = mikrio::PutBucketOwnershipControlsInput::builder(
                values::string(bucket)?,
                ownership,
            );

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_bucket_ownership_controls(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::PutBucketOwnershipControlsOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_ownership_controls(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::GetBucketOwnershipControlsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetBucketOwnershipControlsInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_ownership_controls(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(access_configuration::GetBucketOwnershipControlsOutput {
            object_ownership: match output.object_ownership() {
                mikrio::ObjectOwnership::BucketOwnerEnforced => "BucketOwnerEnforced",
            },
        })
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_acl(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::GetBucketAclOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::GetBucketAclInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_acl(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        access_configuration::GetBucketAclOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None))]
    fn get_object_acl(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<access_configuration::GetObjectAclOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder =
            mikrio::GetObjectAclInput::builder(values::string(bucket)?, values::string(key)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_object_acl(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        access_configuration::GetObjectAclOutput::new(py, &output)
    }

    #[pyo3(signature = (*, prefix=None, max_buckets=None, continuation_token=None))]
    fn list_buckets(
        &self,
        py: Python<'_>,
        prefix: Option<&Bound<'_, PyString>>,
        max_buckets: Option<&Bound<'_, PyAny>>,
        continuation_token: Option<&Bound<'_, PyString>>,
    ) -> PyResult<listing::ListBucketsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let mut builder = mikrio::ListBucketsInput::builder();

        if let Some(value) = prefix {
            builder = builder.prefix(values::string(value)?);
        }

        if let Some(value) = continuation_token {
            builder = builder.continuation_token(values::string(value)?);
        }

        if let Some(value) = max_buckets {
            let value = u32::try_from(values::integer(value)?).map_err(|_| {
                storage_error(
                    py,
                    "InvalidArgument",
                    "page size is outside the supported range",
                )
            })?;

            builder = builder.max_buckets(value);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.list_buckets(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        listing::ListBucketsOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None, prefix=None, delimiter=None, max_keys=None, continuation_token=None, start_after=None))]
    fn list_objects_v2(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        prefix: Option<&Bound<'_, PyString>>,
        delimiter: Option<&Bound<'_, PyString>>,
        max_keys: Option<&Bound<'_, PyAny>>,
        continuation_token: Option<&Bound<'_, PyString>>,
        start_after: Option<&Bound<'_, PyString>>,
    ) -> PyResult<listing::ListObjectsV2Output> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let mut builder = mikrio::ListObjectsV2Input::builder(values::string(bucket)?);

        if let Some(value) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(value)?);
        }

        if let Some(value) = prefix {
            builder = builder.prefix(values::string(value)?);
        }

        if let Some(value) = delimiter {
            builder = builder.delimiter(values::string(value)?);
        }

        if let Some(value) = start_after {
            builder = builder.start_after(values::string(value)?);
        }

        if let Some(value) = continuation_token {
            builder = builder.continuation_token(values::string(value)?);
        }

        if let Some(value) = max_keys {
            let value = u32::try_from(values::integer(value)?).map_err(|_| {
                storage_error(
                    py,
                    "InvalidArgument",
                    "page size is outside the supported range",
                )
            })?;

            builder = builder.max_keys(value);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.list_objects_v2(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        listing::ListObjectsV2Output::new(py, &output)
    }

    #[pyo3(signature = (*, bucket))]
    fn create_bucket(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
    ) -> PyResult<outputs::CreateBucketOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = mikrio::CreateBucketInput::builder(
            values::string(bucket).map_err(|error| before_mutation(py, error))?,
        )
        .build()
        .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.create_bucket(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(outputs::CreateBucketOutput {})
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn head_bucket(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<outputs::HeadBucketOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let mut builder = mikrio::HeadBucketInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.head_bucket(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(outputs::HeadBucketOutput {
            bucket_region: output.bucket_region().to_owned(),
        })
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn get_bucket_location(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<outputs::GetBucketLocationOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let mut builder = mikrio::GetBucketLocationInput::builder(values::string(bucket)?);

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(account)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.get_bucket_location(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(outputs::GetBucketLocationOutput {
            location_constraint: output.location_constraint().map(str::to_owned),
        })
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None))]
    fn delete_bucket(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<outputs::DeleteBucketOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::DeleteBucketInput::builder(
            values::string(bucket).map_err(|error| before_mutation(py, error))?,
        );

        if let Some(account) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(
                values::string(account).map_err(|error| before_mutation(py, error))?,
            );
        }

        let input = builder
            .build()
            .map_err(|error| mutation_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_bucket(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(outputs::DeleteBucketOutput {})
    }

    #[pyo3(signature = (*, bucket, key, copy_source, expected_bucket_owner=None, expected_source_bucket_owner=None, copy_source_if_match=None, copy_source_if_none_match=None, copy_source_if_modified_since=None, copy_source_if_unmodified_since=None, metadata_directive=None, content_type=None, content_encoding=None, content_disposition=None, content_language=None, cache_control=None, expires=None, metadata=None, tags=None, tagging_directive=None, checksum_algorithm=None, if_match=None, if_none_match=None))]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "PyO3 argument extraction owns the Python borrow"
    )]
    fn copy_object(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        copy_source: PyRef<'_, multipart::ObjectRef>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        expected_source_bucket_owner: Option<&Bound<'_, PyString>>,
        copy_source_if_match: Option<&Bound<'_, PyString>>,
        copy_source_if_none_match: Option<&Bound<'_, PyString>>,
        copy_source_if_modified_since: Option<&Bound<'_, PyAny>>,
        copy_source_if_unmodified_since: Option<&Bound<'_, PyAny>>,
        metadata_directive: Option<&Bound<'_, PyString>>,
        content_type: Option<&Bound<'_, PyString>>,
        content_encoding: Option<&Bound<'_, PyString>>,
        content_disposition: Option<&Bound<'_, PyString>>,
        content_language: Option<&Bound<'_, PyString>>,
        cache_control: Option<&Bound<'_, PyString>>,
        expires: Option<&Bound<'_, PyAny>>,
        metadata: Option<&Bound<'_, PyMapping>>,
        tags: Option<&Bound<'_, PyAny>>,
        tagging_directive: Option<&Bound<'_, PyString>>,
        checksum_algorithm: Option<&Bound<'_, PyString>>,
        if_match: Option<&Bound<'_, PyString>>,
        if_none_match: Option<&Bound<'_, PyString>>,
    ) -> PyResult<multipart::CopyObjectOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::CopyObjectInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                copy_source.value.clone(),
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = expected_source_bucket_owner {
                builder = builder.expected_source_bucket_owner(values::string(value)?);
            }

            if let Some(value) = copy_source_if_match {
                builder = builder.copy_source_if_match(values::string(value)?);
            }

            if let Some(value) = copy_source_if_none_match {
                builder = builder.copy_source_if_none_match(values::string(value)?);
            }

            if let Some(value) = copy_source_if_modified_since {
                builder = builder.copy_source_if_modified_since(values::timestamp(value)?);
            }

            if let Some(value) = copy_source_if_unmodified_since {
                builder = builder.copy_source_if_unmodified_since(values::timestamp(value)?);
            }

            if let Some(value) = metadata_directive {
                builder = builder.metadata_directive(multipart::copy_directive(value)?);
            }

            if let Some(value) = content_type {
                builder = builder.content_type(values::string(value)?);
            }

            if let Some(value) = content_encoding {
                builder = builder.content_encoding(values::string(value)?);
            }

            if let Some(value) = content_disposition {
                builder = builder.content_disposition(values::string(value)?);
            }

            if let Some(value) = content_language {
                builder = builder.content_language(values::string(value)?);
            }

            if let Some(value) = cache_control {
                builder = builder.cache_control(values::string(value)?);
            }

            if let Some(value) = expires {
                builder = builder.expires(values::timestamp(value)?);
            }

            if let Some(value) = metadata {
                builder = builder.metadata(values::metadata(value.as_any())?);
            }

            if let Some(value) = tags {
                builder = builder.tags(values::tags(value)?);
            }

            if let Some(value) = tagging_directive {
                builder = builder.tagging_directive(multipart::copy_directive(value)?);
            }

            if let Some(value) = checksum_algorithm {
                builder = builder.checksum_algorithm(values::checksum_algorithm(value)?);
            }

            if let Some(value) = if_match {
                builder = builder.if_match(values::string(value)?);
            }

            if let Some(value) = if_none_match {
                builder = builder.if_none_match(values::string(value)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.copy_object(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        multipart::CopyObjectOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None, content_type=None, content_encoding=None, content_disposition=None, content_language=None, cache_control=None, expires=None, metadata=None, tags=None, checksum_algorithm=None, checksum_type=None))]
    fn create_multipart_upload(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        content_type: Option<&Bound<'_, PyString>>,
        content_encoding: Option<&Bound<'_, PyString>>,
        content_disposition: Option<&Bound<'_, PyString>>,
        content_language: Option<&Bound<'_, PyString>>,
        cache_control: Option<&Bound<'_, PyString>>,
        expires: Option<&Bound<'_, PyAny>>,
        metadata: Option<&Bound<'_, PyMapping>>,
        tags: Option<&Bound<'_, PyAny>>,
        checksum_algorithm: Option<&Bound<'_, PyString>>,
        checksum_type: Option<&Bound<'_, PyString>>,
    ) -> PyResult<multipart::CreateMultipartUploadOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::CreateMultipartUploadInput::builder(
                values::string(bucket)?,
                values::string(key)?,
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = content_type {
                builder = builder.content_type(values::string(value)?);
            }

            if let Some(value) = content_encoding {
                builder = builder.content_encoding(values::string(value)?);
            }

            if let Some(value) = content_disposition {
                builder = builder.content_disposition(values::string(value)?);
            }

            if let Some(value) = content_language {
                builder = builder.content_language(values::string(value)?);
            }

            if let Some(value) = cache_control {
                builder = builder.cache_control(values::string(value)?);
            }

            if let Some(value) = expires {
                builder = builder.expires(values::timestamp(value)?);
            }

            if let Some(value) = metadata {
                builder = builder.metadata(values::metadata(value.as_any())?);
            }

            if let Some(value) = tags {
                builder = builder.tags(values::tags(value)?);
            }

            if let Some(value) = checksum_algorithm {
                builder = builder.checksum_algorithm(values::checksum_algorithm(value)?);
            }

            if let Some(value) = checksum_type {
                builder = builder.checksum_type(multipart::checksum_type(value)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.create_multipart_upload(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(multipart::CreateMultipartUploadOutput::new(&output))
    }

    #[pyo3(signature = (*, bucket, key, upload_id, part_number, body, expected_bucket_owner=None, content_length=None, checksum_algorithm=None, content_md5=None, checksum_crc32=None, checksum_crc32c=None, checksum_sha1=None, checksum_sha256=None, checksum_crc64_nvme=None))]
    fn upload_part<'py>(
        &self,
        py: Python<'py>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        upload_id: &Bound<'_, PyString>,
        part_number: &Bound<'_, PyAny>,
        body: &Bound<'py, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        content_length: Option<&Bound<'_, PyAny>>,
        checksum_algorithm: Option<&Bound<'_, PyString>>,
        content_md5: Option<&Bound<'_, PyString>>,
        checksum_crc32: Option<&Bound<'_, PyString>>,
        checksum_crc32c: Option<&Bound<'_, PyString>>,
        checksum_sha1: Option<&Bound<'_, PyString>>,
        checksum_sha256: Option<&Bound<'_, PyString>>,
        checksum_crc64_nvme: Option<&Bound<'_, PyString>>,
    ) -> PyResult<multipart::UploadPartOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let (upload, source) = mikrio_embed::upload::Upload::channel();

        let input = (|| {
            let mut builder = mikrio::UploadPartInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                values::string(upload_id)?,
                multipart::count(part_number)?,
                source,
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = content_length {
                builder = builder.content_length(values::integer(value)?);
            }

            if let Some(value) = checksum_algorithm {
                builder = builder.checksum_algorithm(values::checksum_algorithm(value)?);
            }

            if let Some(value) = content_md5 {
                builder = builder.content_md5(values::string(value)?);
            }

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

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        upload::part(py, owner, input, upload, body)
    }

    #[pyo3(signature = (*, bucket, key, upload_id, part_number, copy_source, expected_bucket_owner=None, expected_source_bucket_owner=None, copy_source_if_match=None, copy_source_if_none_match=None, copy_source_if_modified_since=None, copy_source_if_unmodified_since=None, copy_source_range=None))]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "PyO3 argument extraction owns the Python borrow"
    )]
    fn upload_part_copy(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        upload_id: &Bound<'_, PyString>,
        part_number: &Bound<'_, PyAny>,
        copy_source: PyRef<'_, multipart::ObjectRef>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        expected_source_bucket_owner: Option<&Bound<'_, PyString>>,
        copy_source_if_match: Option<&Bound<'_, PyString>>,
        copy_source_if_none_match: Option<&Bound<'_, PyString>>,
        copy_source_if_modified_since: Option<&Bound<'_, PyAny>>,
        copy_source_if_unmodified_since: Option<&Bound<'_, PyAny>>,
        copy_source_range: Option<&Bound<'_, PyString>>,
    ) -> PyResult<multipart::UploadPartCopyOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::UploadPartCopyInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                values::string(upload_id)?,
                multipart::count(part_number)?,
                copy_source.value.clone(),
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = expected_source_bucket_owner {
                builder = builder.expected_source_bucket_owner(values::string(value)?);
            }

            if let Some(value) = copy_source_if_match {
                builder = builder.copy_source_if_match(values::string(value)?);
            }

            if let Some(value) = copy_source_if_none_match {
                builder = builder.copy_source_if_none_match(values::string(value)?);
            }

            if let Some(value) = copy_source_if_modified_since {
                builder = builder.copy_source_if_modified_since(values::timestamp(value)?);
            }

            if let Some(value) = copy_source_if_unmodified_since {
                builder = builder.copy_source_if_unmodified_since(values::timestamp(value)?);
            }

            if let Some(value) = copy_source_range {
                builder = builder.copy_source_range(values::string(value)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.upload_part_copy(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        multipart::UploadPartCopyOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, upload_id, parts, expected_bucket_owner=None, if_match=None, if_none_match=None, content_length=None, checksum_type=None, checksum_crc32=None, checksum_crc32c=None, checksum_sha1=None, checksum_sha256=None, checksum_crc64_nvme=None))]
    fn complete_multipart_upload(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        upload_id: &Bound<'_, PyString>,
        parts: &Bound<'_, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        if_match: Option<&Bound<'_, PyString>>,
        if_none_match: Option<&Bound<'_, PyString>>,
        content_length: Option<&Bound<'_, PyAny>>,
        checksum_type: Option<&Bound<'_, PyString>>,
        checksum_crc32: Option<&Bound<'_, PyString>>,
        checksum_crc32c: Option<&Bound<'_, PyString>>,
        checksum_sha1: Option<&Bound<'_, PyString>>,
        checksum_sha256: Option<&Bound<'_, PyString>>,
        checksum_crc64_nvme: Option<&Bound<'_, PyString>>,
    ) -> PyResult<multipart::CompleteMultipartUploadOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::CompleteMultipartUploadInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                values::string(upload_id)?,
                multipart::parts(parts)?,
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = if_match {
                builder = builder.if_match(values::string(value)?);
            }

            if let Some(value) = if_none_match {
                builder = builder.if_none_match(values::string(value)?);
            }

            if let Some(value) = content_length {
                builder = builder.content_length(values::integer(value)?);
            }

            if let Some(value) = checksum_type {
                builder = builder.checksum_type(multipart::checksum_type(value)?);
            }

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

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.complete_multipart_upload(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(multipart::CompleteMultipartUploadOutput::new(&output))
    }

    #[pyo3(signature = (*, bucket, key, upload_id, expected_bucket_owner=None))]
    fn abort_multipart_upload(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        upload_id: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
    ) -> PyResult<multipart::AbortMultipartUploadOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder = mikrio::AbortMultipartUploadInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                values::string(upload_id)?,
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.abort_multipart_upload(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(multipart::AbortMultipartUploadOutput::new(output))
    }

    #[pyo3(signature = (*, bucket, key, upload_id, expected_bucket_owner=None, part_number_marker=None, max_parts=None))]
    fn list_parts(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        upload_id: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        part_number_marker: Option<&Bound<'_, PyAny>>,
        max_parts: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<multipart::ListPartsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::ListPartsInput::builder(
            values::string(bucket)?,
            values::string(key)?,
            values::string(upload_id)?,
        );

        if let Some(value) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(value)?);
        }

        if let Some(value) = part_number_marker {
            builder = builder.part_number_marker(multipart::count(value)?);
        }

        if let Some(value) = max_parts {
            builder = builder.max_parts(multipart::count(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.list_parts(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        multipart::ListPartsOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, expected_bucket_owner=None, prefix=None, delimiter=None, key_marker=None, upload_id_marker=None, max_uploads=None))]
    fn list_multipart_uploads(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        prefix: Option<&Bound<'_, PyString>>,
        delimiter: Option<&Bound<'_, PyString>>,
        key_marker: Option<&Bound<'_, PyString>>,
        upload_id_marker: Option<&Bound<'_, PyString>>,
        max_uploads: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<multipart::ListMultipartUploadsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder = mikrio::ListMultipartUploadsInput::builder(values::string(bucket)?);

        if let Some(value) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(value)?);
        }

        if let Some(value) = prefix {
            builder = builder.prefix(values::string(value)?);
        }

        if let Some(value) = delimiter {
            builder = builder.delimiter(values::string(value)?);
        }

        if let Some(value) = key_marker {
            builder = builder.key_marker(values::string(value)?);
        }

        if let Some(value) = upload_id_marker {
            builder = builder.upload_id_marker(values::string(value)?);
        }

        if let Some(value) = max_uploads {
            builder = builder.max_uploads(multipart::count(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.list_multipart_uploads(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        multipart::ListMultipartUploadsOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, body, expected_bucket_owner=None, content_length=None, content_type=None, content_encoding=None, content_disposition=None, content_language=None, cache_control=None, expires=None, metadata=None, tags=None, checksum_algorithm=None, content_md5=None, checksum_crc32=None, checksum_crc32c=None, checksum_sha1=None, checksum_sha256=None, checksum_crc64_nvme=None, if_match=None, if_none_match=None))]
    fn put_object<'py>(
        &self,
        py: Python<'py>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        body: &Bound<'py, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        content_length: Option<&Bound<'_, PyAny>>,
        content_type: Option<&Bound<'_, PyString>>,
        content_encoding: Option<&Bound<'_, PyString>>,
        content_disposition: Option<&Bound<'_, PyString>>,
        content_language: Option<&Bound<'_, PyString>>,
        cache_control: Option<&Bound<'_, PyString>>,
        expires: Option<&Bound<'_, PyAny>>,
        metadata: Option<&Bound<'_, PyDict>>,
        tags: Option<&Bound<'_, PyAny>>,
        checksum_algorithm: Option<&Bound<'_, PyString>>,
        content_md5: Option<&Bound<'_, PyString>>,
        checksum_crc32: Option<&Bound<'_, PyString>>,
        checksum_crc32c: Option<&Bound<'_, PyString>>,
        checksum_sha1: Option<&Bound<'_, PyString>>,
        checksum_sha256: Option<&Bound<'_, PyString>>,
        checksum_crc64_nvme: Option<&Bound<'_, PyString>>,
        if_match: Option<&Bound<'_, PyString>>,
        if_none_match: Option<&Bound<'_, PyString>>,
    ) -> PyResult<outputs::PutObjectOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let (upload, source) = mikrio_embed::upload::Upload::channel();

        let input = (|| {
            let mut builder = mikrio::PutObjectInput::builder(
                values::string(bucket)?,
                values::string(key)?,
                source,
            );

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = content_length {
                builder = builder.content_length(values::integer(value)?);
            }

            if let Some(value) = content_type {
                builder = builder.content_type(values::string(value)?);
            }

            if let Some(value) = content_encoding {
                builder = builder.content_encoding(values::string(value)?);
            }

            if let Some(value) = content_disposition {
                builder = builder.content_disposition(values::string(value)?);
            }

            if let Some(value) = content_language {
                builder = builder.content_language(values::string(value)?);
            }

            if let Some(value) = cache_control {
                builder = builder.cache_control(values::string(value)?);
            }

            if let Some(value) = expires {
                builder = builder.expires(values::timestamp(value)?);
            }

            if let Some(value) = metadata {
                builder = builder.metadata(values::metadata(value.as_any())?);
            }

            if let Some(value) = tags {
                builder = builder.tags(values::tags(value)?);
            }

            if let Some(value) = checksum_algorithm {
                builder = builder.checksum_algorithm(values::checksum_algorithm(value)?);
            }

            if let Some(value) = content_md5 {
                builder = builder.content_md5(values::string(value)?);
            }

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

            if let Some(value) = if_match {
                builder = builder.if_match(values::string(value)?);
            }

            if let Some(value) = if_none_match {
                builder = builder.if_none_match(values::string(value)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        upload::put(py, owner, input, upload, body)
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None, checksum_mode=None, range=None, part_number=None, if_match=None, if_none_match=None, if_modified_since=None, if_unmodified_since=None, response_content_type=None, response_content_encoding=None, response_content_disposition=None, response_content_language=None, response_cache_control=None, response_expires=None))]
    fn get_object(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        checksum_mode: Option<&Bound<'_, PyString>>,
        range: Option<&Bound<'_, PyString>>,
        part_number: Option<&Bound<'_, PyAny>>,
        if_match: Option<&Bound<'_, PyString>>,
        if_none_match: Option<&Bound<'_, PyString>>,
        if_modified_since: Option<&Bound<'_, PyAny>>,
        if_unmodified_since: Option<&Bound<'_, PyAny>>,
        response_content_type: Option<&Bound<'_, PyString>>,
        response_content_encoding: Option<&Bound<'_, PyString>>,
        response_content_disposition: Option<&Bound<'_, PyString>>,
        response_content_language: Option<&Bound<'_, PyString>>,
        response_cache_control: Option<&Bound<'_, PyString>>,
        response_expires: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<outputs::GetObjectOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder =
            mikrio::GetObjectInput::builder(values::string(bucket)?, values::string(key)?);

        if let Some(value) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(value)?);
        }

        if let Some(value) = checksum_mode {
            builder = builder.checksum_mode(values::checksum_mode(value)?);
        }

        if let Some(value) = range {
            builder = builder.range(values::string(value)?);
        }

        if let Some(value) = part_number {
            builder = builder.part_number(multipart::count(value)?);
        }

        if let Some(value) = response_content_type {
            builder = builder.response_content_type(values::string(value)?);
        }

        if let Some(value) = response_content_encoding {
            builder = builder.response_content_encoding(values::string(value)?);
        }

        if let Some(value) = response_content_disposition {
            builder = builder.response_content_disposition(values::string(value)?);
        }

        if let Some(value) = response_content_language {
            builder = builder.response_content_language(values::string(value)?);
        }

        if let Some(value) = response_cache_control {
            builder = builder.response_cache_control(values::string(value)?);
        }

        if let Some(value) = response_expires {
            builder = builder.response_expires(values::timestamp(value)?);
        }

        if let Some(value) = if_match {
            builder = builder.if_match(values::string(value)?);
        }

        if let Some(value) = if_none_match {
            builder = builder.if_none_match(values::string(value)?);
        }

        if let Some(value) = if_modified_since {
            builder = builder.if_modified_since(values::timestamp(value)?);
        }

        if let Some(value) = if_unmodified_since {
            builder = builder.if_unmodified_since(values::timestamp(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move {
                let output = store.get_object(input).await?;
                let fields = outputs::ReadDetails::from(&output);
                let length = output.content_length();
                let body = mikrio_embed::body::Body::new(output.into_body(), length);

                Ok::<_, mikrio::StorageError>((fields, body))
            })
            .map_err(|error| host_error(&error))?;

        let (fields, body) = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        let body = Py::new(py, body::ObjectBody::new(body, Arc::clone(&owner.runtime)))?;
        outputs::GetObjectOutput::new(py, &fields, body)
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None, checksum_mode=None, range=None, part_number=None, if_match=None, if_none_match=None, if_modified_since=None, if_unmodified_since=None, response_content_type=None, response_content_encoding=None, response_content_disposition=None, response_content_language=None, response_cache_control=None, response_expires=None))]
    fn head_object(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        checksum_mode: Option<&Bound<'_, PyString>>,
        range: Option<&Bound<'_, PyString>>,
        part_number: Option<&Bound<'_, PyAny>>,
        if_match: Option<&Bound<'_, PyString>>,
        if_none_match: Option<&Bound<'_, PyString>>,
        if_modified_since: Option<&Bound<'_, PyAny>>,
        if_unmodified_since: Option<&Bound<'_, PyAny>>,
        response_content_type: Option<&Bound<'_, PyString>>,
        response_content_encoding: Option<&Bound<'_, PyString>>,
        response_content_disposition: Option<&Bound<'_, PyString>>,
        response_content_language: Option<&Bound<'_, PyString>>,
        response_cache_control: Option<&Bound<'_, PyString>>,
        response_expires: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<outputs::HeadObjectOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let mut builder =
            mikrio::HeadObjectInput::builder(values::string(bucket)?, values::string(key)?);

        if let Some(value) = expected_bucket_owner {
            builder = builder.expected_bucket_owner(values::string(value)?);
        }

        if let Some(value) = checksum_mode {
            builder = builder.checksum_mode(values::checksum_mode(value)?);
        }

        if let Some(value) = range {
            builder = builder.range(values::string(value)?);
        }

        if let Some(value) = part_number {
            builder = builder.part_number(multipart::count(value)?);
        }

        if let Some(value) = response_content_type {
            builder = builder.response_content_type(values::string(value)?);
        }

        if let Some(value) = response_content_encoding {
            builder = builder.response_content_encoding(values::string(value)?);
        }

        if let Some(value) = response_content_disposition {
            builder = builder.response_content_disposition(values::string(value)?);
        }

        if let Some(value) = response_content_language {
            builder = builder.response_content_language(values::string(value)?);
        }

        if let Some(value) = response_cache_control {
            builder = builder.response_cache_control(values::string(value)?);
        }

        if let Some(value) = response_expires {
            builder = builder.response_expires(values::timestamp(value)?);
        }

        if let Some(value) = if_match {
            builder = builder.if_match(values::string(value)?);
        }

        if let Some(value) = if_none_match {
            builder = builder.if_none_match(values::string(value)?);
        }

        if let Some(value) = if_modified_since {
            builder = builder.if_modified_since(values::timestamp(value)?);
        }

        if let Some(value) = if_unmodified_since {
            builder = builder.if_unmodified_since(values::timestamp(value)?);
        }

        let input = builder.build().map_err(|error| native_error(py, &error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.head_object(input).await })
            .map_err(|error| host_error(&error))?;

        let output = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        outputs::HeadObjectOutput::new(py, &outputs::ReadDetails::from(&output))
    }

    #[pyo3(signature = (*, bucket, objects, expected_bucket_owner=None, quiet=None))]
    fn delete_objects(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        objects: &Bound<'_, PyAny>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        quiet: Option<&Bound<'_, PyBool>>,
    ) -> PyResult<deletion::DeleteObjectsOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut entries = Vec::new();

            for object in objects.try_iter()? {
                if entries.len() == 1000 {
                    return Err(crate::storage_error(
                        py,
                        "InvalidArgument",
                        "delete batch exceeds 1000 entries",
                    ));
                }

                entries.push(
                    object?
                        .extract::<PyRef<'_, deletion::ObjectIdentifier>>()?
                        .inner
                        .clone(),
                );
            }

            let mut builder = mikrio::DeleteObjectsInput::builder(values::string(bucket)?, entries)
                .quiet(quiet.is_some_and(pyo3::types::PyBoolMethods::is_true));

            if let Some(account) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(account)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_objects(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        let output = mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        deletion::DeleteObjectsOutput::new(py, &output)
    }

    #[pyo3(signature = (*, bucket, key, expected_bucket_owner=None, if_match=None))]
    fn delete_object(
        &self,
        py: Python<'_>,
        bucket: &Bound<'_, PyString>,
        key: &Bound<'_, PyString>,
        expected_bucket_owner: Option<&Bound<'_, PyString>>,
        if_match: Option<&Bound<'_, PyString>>,
    ) -> PyResult<outputs::DeleteObjectOutput> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let input = (|| {
            let mut builder =
                mikrio::DeleteObjectInput::builder(values::string(bucket)?, values::string(key)?);

            if let Some(value) = expected_bucket_owner {
                builder = builder.expected_bucket_owner(values::string(value)?);
            }

            if let Some(value) = if_match {
                builder = builder.if_match(values::string(value)?);
            }

            builder.build().map_err(|error| native_error(py, &error))
        })()
        .map_err(|error| before_mutation(py, error))?;

        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.delete_object(input).await })
            .map_err(|error| before_mutation(py, host_error(&error)))?;

        mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))?;

        Ok(outputs::DeleteObjectOutput {})
    }
}

fn context_exit(
    py: Python<'_>,
    exception: &Bound<'_, PyAny>,
    closed: PyResult<()>,
) -> PyResult<bool> {
    let Err(error) = closed else {
        return Ok(false);
    };

    if exception.is_none() {
        return Err(error);
    }

    let primary = PyErr::from_value(exception.cast::<PyBaseException>()?.clone().into_any());
    chain_cleanup_error(py, &primary, error);
    Ok(false)
}

fn chain_cleanup_error(py: Python<'_>, primary: &PyErr, cleanup: PyErr) {
    let previous = primary.context(py);
    let mut context = Some(primary.clone_ref(py));
    let mut seen = HashSet::new();

    // Reused cleanup exceptions may already occur deeper in the chain. Adding
    // one again would replace its earlier context and create a cycle. Existing
    // user-supplied cycles must also terminate this traversal.
    while let Some(error) = context {
        if !seen.insert(error.value(py).as_ptr()) {
            break;
        }

        if error.value(py).is(cleanup.value(py)) {
            return;
        }

        context = error.context(py);
    }

    // Use the exception API rather than invoking a user-defined __setattr__.
    // Keep the original cause and prior context behind the secondary failure;
    // a cleanup-time signal may itself have acquired the primary as its context.
    cleanup.set_context(py, previous);
    primary.set_context(py, Some(cleanup));
}

fn wait_close(py: Python<'_>, pending: &mikrio_embed::CloseWait) -> PyResult<()> {
    loop {
        py.check_signals()?;

        let Some(outcome) = py
            .detach(|| pending.wait(Duration::from_millis(50)))
            .map_err(|error| host_error(&error))?
        else {
            continue;
        };

        return match outcome.as_ref() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(native_error(py, error)),
            Err(error) => Err(host_error(error)),
        };
    }
}

fn native_error(py: Python<'_>, cause: &mikrio::StorageError) -> PyErr {
    let error = storage_error(py, cause.code().as_str(), &cause.to_string());

    if let Some(progress) = cause.partial_result() {
        let projected = deletion::DeleteObjectsProgress::new(py, progress)
            .and_then(|progress| Py::new(py, progress))
            .and_then(|progress| error.value(py).setattr("partial_result", progress));

        if let Err(failure) = projected {
            return failure;
        }
    }

    if let Some(outcome) = cause.mutation_outcome() {
        let converted = error
            .value(py)
            .setattr("mutation_outcome", outcome.as_str());

        if let Err(error) = converted {
            return error;
        }
    }

    let mut parent = error.value(py).clone().into_any();
    let mut source = cause.source();

    // The engine's typed chains are short. Bound detached presentation too;
    // never expose this trusted local context in the HTTP error serializer.
    for _ in 0..16 {
        let Some(current) = source else { break };

        let converted = if let Some(io) = current.downcast_ref::<std::io::Error>() {
            PyErr::from(std::io::Error::new(io.kind(), io.to_string()))
        } else {
            PyException::new_err(current.to_string())
        };

        if let Err(failure) = parent.setattr("__cause__", converted.value(py)) {
            return failure;
        }

        parent = converted.value(py).clone().into_any();
        source = current.source();
    }

    error
}

fn mutation_error(py: Python<'_>, cause: &mikrio::StorageError) -> PyErr {
    before_mutation(py, native_error(py, cause))
}

fn before_mutation(py: Python<'_>, error: PyErr) -> PyErr {
    if error.is_instance_of::<StorageError>(py)
        && let Err(error) = error.value(py).setattr("mutation_outcome", "NotCommitted")
    {
        return error;
    }

    error
}

fn mutation_wait<T: Send>(py: Python<'_>, pending: Pending<T>) -> PyResult<T> {
    wait(py, pending).map_err(|error| uncertain_mutation(py, error))
}

fn uncertain_mutation(py: Python<'_>, error: PyErr) -> PyErr {
    if error.is_instance_of::<StorageError>(py)
        && let Err(error) = error.value(py).setattr("mutation_outcome", "Unknown")
    {
        return error;
    }

    error
}

fn wait_transfer<F>(py: Python<'_>, runtime: &HostRuntime, future: F) -> PyResult<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    let mut future = std::pin::pin!(future);

    loop {
        check_process().map_err(|error| host_error(&error))?;
        py.check_signals()?;

        if let Some(value) = py
            .detach(|| runtime.wait_transfer(future.as_mut(), Duration::from_millis(50)))
            .map_err(|error| host_error(&error))?
        {
            return Ok(value);
        }
    }
}

fn wait<T: Send>(py: Python<'_>, mut pending: Pending<T>) -> PyResult<T> {
    loop {
        check_process().map_err(|error| host_error(&error))?;
        py.check_signals()?;

        if let Some(value) = py
            .detach(|| pending.wait(Duration::from_millis(50)))
            .map_err(|error| host_error(&error))?
        {
            return Ok(value);
        }
    }
}

fn storage_error(py: Python<'_>, code: &str, message: &str) -> PyErr {
    let error = StorageError::new_err(message.to_owned());
    let value = error.value(py);

    let attributes = value
        .setattr("code", code)
        .and_then(|()| value.setattr("mutation_outcome", py.None()))
        .and_then(|()| value.setattr("partial_result", py.None()));

    match attributes {
        Ok(()) => error,
        Err(error) => error,
    }
}

fn host_error(error: &HostError) -> PyErr {
    match error {
        HostError::InheritedProcess => {
            PyRuntimeError::new_err("native state inherited after fork; use spawn or exec")
        }
        HostError::Busy => {
            Python::attach(|py| storage_error(py, "StoreBusy", "native host admission is busy"))
        }
        HostError::Unavailable(cause) => Python::attach(|py| {
            let error = storage_error(py, "IoError", "native host runtime is unavailable");
            error.set_cause(py, Some(PyOSError::new_err(cause.to_string())));
            error
        }),
        HostError::Panicked => PyRuntimeError::new_err("native operation panicked"),
    }
}

#[pymodule(gil_used = true)]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Store>()?;
    listing::register(module)?;
    deletion::register(module)?;
    module.add_class::<StoreInfo>()?;
    outputs::register(module)?;
    multipart::register(module)?;
    admin::register(module)?;
    access_configuration::register(module)?;
    cors::register(module)?;
    lifecycle::register(module)?;
    object_attributes::register(module)?;
    object_tagging::register(module)?;
    listener::register(module)?;
    module.add_class::<body::ObjectBody>()?;
    module.add("StorageError", module.py().get_type::<StorageError>())?;

    Ok(())
}
