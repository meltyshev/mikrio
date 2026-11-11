//! Per-owner unit-test faults around real effects. No feature, environment
//! switch, or ordinary-library symbol can enable this module.

use std::fmt;
use std::io;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Effect {
    FileSync,
    #[cfg(target_os = "macos")]
    DeviceSync,
    Rename,
    DestinationSync,
    SourceSync,
    Unlink,
    UnlinkSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Before,
    After,
}

#[derive(Default)]
pub(super) struct Faults(Mutex<State>);

#[derive(Default)]
struct State {
    failure: Option<(Effect, Phase)>,
    trace: Vec<(Effect, Phase)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Failure(pub(super) Effect, pub(super) Phase);

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "injected native durability failure {:?} {:?}",
            self.0, self.1
        )
    }
}

impl std::error::Error for Failure {}

pub(super) fn observe(observer: Option<&Faults>, effect: Effect, phase: Phase) -> io::Result<()> {
    if let Some(observer) = observer {
        observer.at(effect, phase)?;
    }

    Ok(())
}

impl Faults {
    pub(super) fn arm(&self, failure: Option<(Effect, Phase)>) {
        *self.0.lock().unwrap() = State {
            failure,
            trace: Vec::new(),
        };
    }

    pub(super) fn trace(&self) -> Vec<(Effect, Phase)> {
        self.0.lock().unwrap().trace.clone()
    }

    pub(super) fn at(&self, effect: Effect, phase: Phase) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.trace.push((effect, phase));

        if state.failure == Some((effect, phase)) {
            return Err(io::Error::other(Failure(effect, phase)));
        }

        drop(state);
        crate::protocol_fault::hit("fs", &format!("{effect:?}.{phase:?}"))
    }
}
