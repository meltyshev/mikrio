//! Listener values and synchronous waits use the existing bounded host runtime.

use std::sync::Arc;

use mikrio_embed::{CloseGate, HostRuntime, ProcessBound};
use pyo3::prelude::*;
use pyo3::types::PyString;

use crate::{
    Store, context_exit, host_error, native_error, storage_error, values, wait, wait_close,
};

/// Detached listener signing credentials; the secret is omitted from repr.
#[pyclass(frozen, module = "mikrio")]
#[derive(Clone)]
pub(crate) struct Credentials {
    value: mikrio::Credentials,
}

#[pymethods]
impl Credentials {
    #[new]
    #[pyo3(signature = (*, access_key_id, secret_access_key))]
    fn new(
        py: Python<'_>,
        access_key_id: &Bound<'_, PyString>,
        secret_access_key: &Bound<'_, PyString>,
    ) -> PyResult<Self> {
        let value = mikrio::Credentials::new(
            values::string(access_key_id)?,
            values::string(secret_access_key)?,
        )
        .map_err(|error| native_error(py, &error))?;

        Ok(Self { value })
    }

    #[getter]
    fn access_key_id(&self) -> &str {
        self.value.access_key_id()
    }

    #[getter]
    fn secret_access_key(&self) -> &str {
        self.value.secret_access_key()
    }

    fn __repr__(&self) -> String {
        format!(
            "Credentials(access_key_id={:?}, secret_access_key=[REDACTED])",
            self.value.access_key_id()
        )
    }
}

struct Owner {
    server: mikrio::Server,
    runtime: Arc<HostRuntime>,
    close: CloseGate,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let server = self.server.clone();

        // Retain the host executor until real socket/native cleanup settles.
        let _cleanup = self
            .close
            .begin(&self.runtime, async move { server.close().await });
    }
}

/// Owned HTTP listener with connection details that remain readable after close.
/// Use explicit close or a context manager; snapshots do not imply a live endpoint.
#[pyclass(frozen, module = "mikrio")]
pub(crate) struct Server {
    owner: ProcessBound<Owner>,
}

#[pymethods]
impl Server {
    #[getter]
    fn endpoint(&self) -> PyResult<String> {
        Ok(self
            .owner
            .get()
            .map_err(|error| host_error(&error))?
            .server
            .endpoint()
            .to_owned())
    }

    #[getter]
    fn region(&self) -> PyResult<String> {
        Ok(self
            .owner
            .get()
            .map_err(|error| host_error(&error))?
            .server
            .region()
            .to_owned())
    }

    #[getter]
    fn credentials(&self) -> PyResult<Credentials> {
        Ok(Credentials {
            value: self
                .owner
                .get()
                .map_err(|error| host_error(&error))?
                .server
                .credentials()
                .clone(),
        })
    }

    /// Stop accepting requests and settle this listener's work without closing the store.
    /// Repeated calls join shutdown; a failed close can be retried.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let server = owner.server.clone();

        let pending = owner
            .close
            .begin(&owner.runtime, async move { server.close().await })
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
}

pub(crate) fn listen(
    store: &Store,
    py: Python<'_>,
    host: Option<&Bound<'_, PyString>>,
    port: Option<&Bound<'_, PyAny>>,
    public_endpoint: Option<&Bound<'_, PyString>>,
    credentials: Option<&Credentials>,
) -> PyResult<Server> {
    let owner = store.owner.get().map_err(|error| host_error(&error))?;
    let mut options = mikrio::ListenOptions::default();

    if let Some(host) = host {
        options = options.host(values::string(host)?);
    }

    if let Some(port) = port {
        let port = u16::try_from(values::integer(port)?).map_err(|_| {
            storage_error(py, "InvalidArgument", "port must be between 0 and 65535")
        })?;

        options = options.port(port);
    }

    if let Some(origin) = public_endpoint {
        options = options.public_endpoint(values::string(origin)?);
    }

    if let Some(credentials) = credentials {
        options = options.credentials(credentials.value.clone());
    }

    let store = owner.store.clone();

    let pending = owner
        .runtime
        .submit(async move { store.listen(options).await })
        .map_err(|error| host_error(&error))?;

    let server = wait(py, pending)?.map_err(|error| native_error(py, &error))?;

    Ok(Server {
        owner: ProcessBound::new(Owner {
            server,
            runtime: Arc::clone(&owner.runtime),
            close: CloseGate::default(),
        }),
    })
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Credentials>()?;
    module.add_class::<Server>()
}
