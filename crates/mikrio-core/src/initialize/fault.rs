//! Test-binary-only seams at actual initializer effects. No environment control
//! or callback is compiled into ordinary library artifacts.

use std::cell::Cell;
use std::io;

use super::MarkerPublication;

type Callback = fn(&str) -> io::Result<()>;

thread_local! {
    static CALLBACK: Cell<Option<Callback>> = const { Cell::new(None) };
}

pub(crate) struct Scope(Option<Callback>);

pub(crate) fn install(callback: Callback) -> Scope {
    Scope(CALLBACK.replace(Some(callback)))
}

impl Drop for Scope {
    fn drop(&mut self) {
        CALLBACK.set(self.0);
    }
}

pub(crate) fn hit(point: &str) -> io::Result<()> {
    CALLBACK.get().map_or(Ok(()), |callback| callback(point))
}

pub(super) fn marker(publication: MarkerPublication, effect: &str) -> io::Result<()> {
    let protocol = match publication {
        MarkerPublication::Initial => "initializing",
        MarkerPublication::Ready => "ready",
    };

    hit(&format!("{protocol}.marker.{effect}"))
}
