//! Stream ownership test extension, absent from the ordinary wheel.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mikrio_embed::{CloseGate, HostError, HostRuntime, Pending, ProcessBound};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

#[path = "../../mikrio-embed/tests/support/streams.rs"]
mod fixture;

create_exception!(_mikrio_stream_probe, ProbeError, PyException);

struct Owner {
    stream: Arc<fixture::Stream>,
    runtime: Arc<HostRuntime>,
    close: CloseGate<fixture::Failure>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _failure = self.stream.fail(fixture::Failure::Closed);
    }
}

#[pyclass(frozen, module = "_mikrio_stream_probe")]
struct StreamProbe(ProcessBound<Owner>);

#[pymethods]
impl StreamProbe {
    #[new]
    #[pyo3(signature = (expected=None))]
    fn new(py: Python<'_>, expected: Option<u64>) -> PyResult<Self> {
        let runtime = py
            .detach(HostRuntime::shared)
            .map_err(|error| host_error(&error))?;

        Ok(Self(ProcessBound::new(Owner {
            stream: fixture::Stream::new(expected),
            runtime,
            close: CloseGate::default(),
        })))
    }

    fn reserve(&self, py: Python<'_>) -> PyResult<Reservation> {
        let owner = self.0.get().map_err(|error| host_error(&error))?;
        let stream = Arc::clone(&owner.stream);

        let pending = owner
            .runtime
            .submit(async move { stream.reserve().await })
            .map_err(|error| host_error(&error))?;

        let reservation = wait(py, pending)?.map_err(|error| failure(py, error))?;

        Ok(Reservation(ProcessBound::new(Mutex::new(Some(
            reservation,
        )))))
    }

    fn read_chunk<'py>(
        &self,
        py: Python<'py>,
        max_bytes: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let owner = self.0.get().map_err(|error| host_error(&error))?;
        let stream = Arc::clone(&owner.stream);

        let pending = owner
            .runtime
            .submit(async move { stream.read(max_bytes).await })
            .map_err(|error| host_error(&error))?;

        Ok(wait(py, pending)?
            .map_err(|error| failure(py, error))?
            .map(|bytes| PyBytes::new(py, &bytes)))
    }

    fn read_all<'py>(&self, py: Python<'py>, max_bytes: usize) -> PyResult<Bound<'py, PyBytes>> {
        let owner = self.0.get().map_err(|error| host_error(&error))?;

        let mut collection = owner
            .stream
            .collection(max_bytes)
            .map_err(|error| failure(py, error))?;

        PyBytes::new_with(py, collection.length, |output| {
            let mut offset: usize = 0;

            loop {
                let pending = owner
                    .runtime
                    .submit(async move {
                        let next = collection.next().await;

                        (collection, next)
                    })
                    .map_err(|error| host_error(&error))?;

                let (returned, next) = wait(py, pending)?;
                collection = returned;

                let Some(bytes) = next.map_err(|error| failure(py, error))? else {
                    if offset != output.len() {
                        return Err(failure(py, fixture::Failure::LengthMismatch));
                    }

                    return Ok(());
                };

                let end = offset
                    .checked_add(bytes.len())
                    .ok_or_else(|| failure(py, fixture::Failure::TooLarge))?;

                let target = output
                    .get_mut(offset..end)
                    .ok_or_else(|| failure(py, fixture::Failure::LengthMismatch))?;

                target.copy_from_slice(&bytes);
                offset = end;
            }
        })
    }

    fn end(&self, py: Python<'_>) -> PyResult<()> {
        self.0
            .get()
            .map_err(|error| host_error(&error))?
            .stream
            .end()
            .map_err(|error| failure(py, error))
    }

    fn fail_producer(&self, py: Python<'_>) -> PyResult<()> {
        self.0
            .get()
            .map_err(|error| host_error(&error))?
            .stream
            .fail(fixture::Failure::Producer)
            .map_err(|error| failure(py, error))
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let owner = self.0.get().map_err(|error| host_error(&error))?;
        let stream = Arc::clone(&owner.stream);

        let pending = owner
            .close
            .begin(&owner.runtime, async move { stream.close().await })
            .map_err(|error| host_error(&error))?;

        loop {
            py.check_signals()?;

            let Some(result) = py
                .detach(|| pending.wait(Duration::from_millis(50)))
                .map_err(|error| host_error(&error))?
            else {
                continue;
            };

            return match result.as_ref() {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(failure(py, *error)),
                Err(error) => Err(PyRuntimeError::new_err(error.to_string())),
            };
        }
    }

    #[getter]
    fn allocations(&self) -> PyResult<(usize, usize)> {
        Ok(self
            .0
            .get()
            .map_err(|error| host_error(&error))?
            .stream
            .allocations())
    }

    #[getter]
    fn is_reading(&self) -> PyResult<bool> {
        Ok(self
            .0
            .get()
            .map_err(|error| host_error(&error))?
            .stream
            .is_reading())
    }
}

#[pyclass(frozen, module = "_mikrio_stream_probe")]
struct Reservation(ProcessBound<Mutex<Option<fixture::Reservation>>>);

#[pymethods]
impl Reservation {
    fn write(&self, py: Python<'_>, bytes: &Bound<'_, PyBytes>) -> PyResult<()> {
        let reservation = self
            .0
            .get()
            .map_err(|error| host_error(&error))?
            .lock()
            .map_err(|_| PyRuntimeError::new_err("poisoned reservation"))?
            .take()
            .ok_or_else(|| failure(py, fixture::Failure::Closed))?;

        // GIL-bound immutable bytes are borrowed only during the bounded copy.
        reservation
            .write(bytes.as_bytes())
            .map_err(|error| failure(py, error))
    }
}

fn wait<T: Send>(py: Python<'_>, mut pending: Pending<T>) -> PyResult<T> {
    loop {
        py.check_signals()?;

        if let Some(value) = py
            .detach(|| pending.wait(Duration::from_millis(50)))
            .map_err(|error| host_error(&error))?
        {
            return Ok(value);
        }
    }
}

fn failure(py: Python<'_>, error: fixture::Failure) -> PyErr {
    let converted = ProbeError::new_err(error.code());

    if let Err(error) = converted.value(py).setattr("code", error.code()) {
        return error;
    }

    converted
}

fn host_error(error: &HostError) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

#[pymodule(gil_used = true)]
fn _mikrio_stream_probe(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<StreamProbe>()?;
    module.add_class::<Reservation>()?;
    module.add("ProbeError", module.py().get_type::<ProbeError>())?;

    Ok(())
}
