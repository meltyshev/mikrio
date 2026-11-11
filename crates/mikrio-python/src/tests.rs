//! Exception chaining uses an explicitly linked interpreter in Rust unit tests.

use pyo3::exceptions::{PyLookupError, PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use super::chain_cleanup_error;

#[test]
fn cleanup_preserves_primary_cause_and_context_without_attribute_hooks() {
    Python::initialize();

    Python::attach(|py| {
        let module = PyModule::from_code(
            py,
            c"class FrozenInterrupt(KeyboardInterrupt):
    def __setattr__(self, name, value):
        raise AssertionError('exception attribute hook must not run')
",
            c"cleanup_fixture.py",
            c"cleanup_fixture",
        )
        .unwrap();

        let interrupt = module.getattr("FrozenInterrupt").unwrap().call0().unwrap();
        let primary = PyErr::from_value(interrupt);
        let cause = PyOSError::new_err("producer cause");
        let previous = PyLookupError::new_err("prior context");
        let cleanup = PyRuntimeError::new_err("iterator close failed");

        primary.set_cause(py, Some(cause.clone_ref(py)));
        primary.set_context(py, Some(previous.clone_ref(py)));
        cleanup.set_context(py, Some(primary.clone_ref(py)));

        chain_cleanup_error(py, &primary, cleanup.clone_ref(py));

        assert!(primary.cause(py).unwrap().value(py).is(cause.value(py)));
        assert!(primary.context(py).unwrap().value(py).is(cleanup.value(py)));

        assert!(
            cleanup
                .context(py)
                .unwrap()
                .value(py)
                .is(previous.value(py))
        );
    });
}

#[test]
fn repeated_cleanup_errors_do_not_create_context_cycles() {
    Python::initialize();

    Python::attach(|py| {
        let primary = PyValueError::new_err("producer failed");
        let previous = PyLookupError::new_err("prior context");
        let cleanup = PyRuntimeError::new_err("iterator close failed");
        primary.set_context(py, Some(previous.clone_ref(py)));

        chain_cleanup_error(py, &primary, primary.clone_ref(py));

        assert!(
            primary
                .context(py)
                .unwrap()
                .value(py)
                .is(previous.value(py))
        );

        chain_cleanup_error(py, &primary, cleanup.clone_ref(py));
        chain_cleanup_error(py, &primary, cleanup.clone_ref(py));

        assert!(primary.context(py).unwrap().value(py).is(cleanup.value(py)));

        assert!(
            cleanup
                .context(py)
                .unwrap()
                .value(py)
                .is(previous.value(py))
        );

        assert!(previous.context(py).is_none());
    });
}

#[test]
fn cleanup_already_in_nested_context_preserves_the_existing_chain() {
    Python::initialize();

    Python::attach(|py| {
        let primary = PyValueError::new_err("producer failed");
        let previous = PyLookupError::new_err("intermediate context");
        let cleanup = PyRuntimeError::new_err("reused cleanup failure");
        let original = PyOSError::new_err("original context");
        primary.set_context(py, Some(previous.clone_ref(py)));
        previous.set_context(py, Some(cleanup.clone_ref(py)));
        cleanup.set_context(py, Some(original.clone_ref(py)));

        chain_cleanup_error(py, &primary, cleanup.clone_ref(py));

        assert!(
            primary
                .context(py)
                .unwrap()
                .value(py)
                .is(previous.value(py))
        );

        assert!(
            previous
                .context(py)
                .unwrap()
                .value(py)
                .is(cleanup.value(py))
        );

        assert!(
            cleanup
                .context(py)
                .unwrap()
                .value(py)
                .is(original.value(py))
        );

        assert!(original.context(py).is_none());
    });
}

#[test]
fn cleanup_chaining_terminates_when_the_primary_context_already_has_a_cycle() {
    Python::initialize();

    Python::attach(|py| {
        let primary = PyValueError::new_err("producer failed");
        let previous = PyLookupError::new_err("cyclic user context");
        let cleanup = PyRuntimeError::new_err("cleanup failed");
        primary.set_context(py, Some(previous.clone_ref(py)));
        previous.set_context(py, Some(primary.clone_ref(py)));

        chain_cleanup_error(py, &primary, cleanup.clone_ref(py));

        assert!(primary.context(py).unwrap().value(py).is(cleanup.value(py)));

        assert!(
            cleanup
                .context(py)
                .unwrap()
                .value(py)
                .is(previous.value(py))
        );

        assert!(
            previous
                .context(py)
                .unwrap()
                .value(py)
                .is(primary.value(py))
        );
    });
}
