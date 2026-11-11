//! Python cursor waits release the interpreter; collection fills one final bytes
//! allocation through bounded native chunks while retaining its exclusive claim.

use std::sync::Arc;

use mikrio_embed::body::{Body, BodyError};
use mikrio_embed::{CloseGate, HostRuntime, ProcessBound};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::{context_exit, host_error, native_error, storage_error, values, wait_transfer};

struct Owner {
    body: Body,
    runtime: Arc<HostRuntime>,
    close: CloseGate<BodyError>,
}

/// One owned response cursor; use explicit close or a context manager for early release.
///
/// Reads release the interpreter while waiting. Concurrent reads raise `BodyInUse`;
/// cancellation or I/O failure makes the cursor terminal.
#[pyclass(frozen, module = "mikrio")]
pub(crate) struct ObjectBody {
    owner: ProcessBound<Owner>,
}

impl ObjectBody {
    pub(crate) fn new(body: Body, runtime: Arc<HostRuntime>) -> Self {
        Self {
            owner: ProcessBound::new(Owner {
                body,
                runtime,
                close: CloseGate::default(),
            }),
        }
    }
}

#[pymethods]
impl ObjectBody {
    /// Read at most `max_bytes` bytes, returning `None` at EOF. The limit must be positive.
    /// Repeated EOF reads return `None`; reads after explicit close raise `BodyClosed`.
    #[pyo3(signature = (*, max_bytes))]
    fn read_chunk<'py>(
        &self,
        py: Python<'py>,
        max_bytes: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;
        let max_bytes = values::bound(max_bytes)?;

        if max_bytes == 0 {
            return Err(storage_error(
                py,
                "InvalidArgument",
                "chunk size must be positive",
            ));
        }

        let mut reading = owner
            .body
            .start_read()
            .map_err(|error| failure(py, &error))?;

        let value = wait_transfer(py, &owner.runtime, reading.next(max_bytes))?
            .map_err(|error| failure(py, &error))?;

        reading.finish();

        Ok(value.map(|bytes| PyBytes::new(py, &bytes)))
    }

    /// Collect the remaining bytes within `max_bytes`, allowing zero for an empty remainder.
    /// Exceeding the limit closes the body and raises `BodyTooLarge` before collection.
    #[pyo3(signature = (*, max_bytes))]
    fn read_all<'py>(
        &self,
        py: Python<'py>,
        max_bytes: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        let (mut reading, length) = owner
            .body
            .collection(values::bound(max_bytes)?)
            .map_err(|error| failure(py, &error))?;

        PyBytes::new_with(py, length, |output| {
            let mut offset = 0usize;

            loop {
                let next = wait_transfer(
                    py,
                    &owner.runtime,
                    reading.next(mikrio_embed::chunks::CHUNK_BYTES),
                )?;

                let Some(bytes) = next.map_err(|error| failure(py, &error))? else {
                    if offset != length {
                        return Err(storage_error(
                            py,
                            "IoError",
                            "object body ended before its selected length",
                        ));
                    }

                    return Ok(());
                };

                let end = offset
                    .checked_add(bytes.len())
                    .ok_or_else(|| storage_error(py, "IoError", "object body length overflow"))?;

                let target = output.get_mut(offset..end).ok_or_else(|| {
                    storage_error(py, "IoError", "object body exceeded its selected length")
                })?;

                target.copy_from_slice(&bytes);
                offset = end;
            }
        })
    }

    /// Cancel a pending read and wait for native release. Repeated close is harmless.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let owner = self.owner.get().map_err(|error| host_error(&error))?;

        owner.body.cancel();

        let body = owner.body.clone();

        let pending = owner
            .close
            .begin(&owner.runtime, async move { body.close().await })
            .map_err(|error| host_error(&error))?;

        loop {
            py.check_signals()?;

            let Some(result) = py
                .detach(|| pending.wait(std::time::Duration::from_millis(50)))
                .map_err(|error| host_error(&error))?
            else {
                continue;
            };

            return match result.as_ref() {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(failure(py, error)),
                Err(error) => Err(host_error(error)),
            };
        }
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

fn failure(py: Python<'_>, error: &BodyError) -> PyErr {
    if let BodyError::Native(error) = error {
        return native_error(py, error);
    }

    storage_error(py, error.code(), &error.to_string())
}
