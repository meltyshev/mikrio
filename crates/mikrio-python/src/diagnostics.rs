//! Interpreter-owned detached delivery. No host callback runs on engine workers.

use mikrio::embedding::Diagnostics;
use mikrio_embed::ProcessBound;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

#[pyclass(frozen, module = "mikrio._native")]
struct DiagnosticReceiver(ProcessBound<Diagnostics>);

#[pymethods]
impl DiagnosticReceiver {
    fn same_queue(&self, other: &Self) -> PyResult<bool> {
        let receiver = self.0.get().map_err(|error| super::host_error(&error))?;
        let other = other.0.get().map_err(|error| super::host_error(&error))?;
        Ok(receiver.same_queue(other))
    }

    fn is_closed(&self) -> PyResult<bool> {
        Ok(self
            .0
            .get()
            .map_err(|error| super::host_error(&error))?
            .is_closed())
    }

    fn take(&self) -> PyResult<Option<String>> {
        Ok(self
            .0
            .get()
            .map_err(|error| super::host_error(&error))?
            .take())
    }

    fn observer_failed(&self) -> PyResult<()> {
        self.0
            .get()
            .map_err(|error| super::host_error(&error))?
            .observer_failed();

        Ok(())
    }
}

pub(super) fn start(py: Python<'_>, diagnostics: &Diagnostics) -> PyResult<()> {
    let receiver = Py::new(
        py,
        DiagnosticReceiver(ProcessBound::new(diagnostics.clone())),
    )?;

    let result = py
        .import("mikrio._diagnostics")?
        .getattr("start")?
        .call1((receiver,));

    match result {
        Ok(_) => Ok(()),
        Err(error) if error.is_instance_of::<PyException>(py) => {
            diagnostics.observer_failed();
            Ok(())
        }
        Err(error) => Err(error),
    }
}
