//! Explicit private diagnostic observation extension, absent from ordinary wheels.

use std::io;

use mikrio_core::StoreId;
use mikrio_core::diagnostics::{Diagnostic, Diagnostics, Work};
use mikrio_embed::ProcessBound;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

struct Owner(Diagnostics);

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[pyclass(frozen, module = "_mikrio_diagnostics_probe")]
struct DiagnosticProbe(ProcessBound<Owner>);

#[pymethods]
impl DiagnosticProbe {
    #[new]
    fn new() -> Self {
        Self(ProcessBound::new(Owner(Diagnostics::default())))
    }

    fn emit(&self, detail: String) -> PyResult<()> {
        let queue = &self
            .0
            .get()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
            .0;

        let error = io::Error::new(io::ErrorKind::PermissionDenied, detail);

        queue.record(Diagnostic::io(
            StoreId::from_bytes([7; 16]),
            Work::Cleanup,
            &error,
        ));

        Ok(())
    }

    fn take(&self) -> PyResult<Option<Event>> {
        let queue = &self
            .0
            .get()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
            .0;

        Ok(queue.take().map(|record| Event {
            store_id: record.store_id().to_string(),
            work: record.work().as_str().into(),
            failure: record.failure().map(str::to_owned),
            occurrences: record.occurrences(),
            age_ms: u64::try_from(record.age().as_millis()).unwrap_or(u64::MAX),
        }))
    }

    fn observer_failed(&self) -> PyResult<()> {
        self.0
            .get()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
            .0
            .observer_failed();

        Ok(())
    }

    fn close(&self) -> PyResult<()> {
        self.0
            .get()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
            .0
            .close();

        Ok(())
    }

    #[getter]
    fn stats(&self) -> PyResult<(usize, u64, u64, u64)> {
        let stats = self
            .0
            .get()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
            .0
            .stats();

        Ok((
            stats.queued,
            stats.dropped,
            stats.coalesced,
            stats.observer_failures,
        ))
    }
}

#[pyclass(frozen, module = "_mikrio_diagnostics_probe", get_all)]
struct Event {
    store_id: String,
    work: String,
    failure: Option<String>,
    occurrences: u64,
    age_ms: u64,
}

#[pymodule(gil_used = true)]
fn _mikrio_diagnostics_probe(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<DiagnosticProbe>()?;
    module.add_class::<Event>()?;

    Ok(())
}
