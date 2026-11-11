//! Owned subprocess-only protocol hooks. Ordinary artifacts have neither this
//! module nor its call sites. The installing child owns its entire process.

use std::cell::Cell;
use std::io;
use std::sync::OnceLock;

type Callback = fn(&str) -> io::Result<()>;
static CALLBACK: OnceLock<Callback> = OnceLock::new();

thread_local! {
    static PROTOCOL: Cell<&'static str> = const { Cell::new("metadata") };
}

pub(crate) struct Scope(&'static str);

impl Scope {
    pub(crate) fn enter(protocol: &'static str) -> Self {
        Self(PROTOCOL.replace(protocol))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        PROTOCOL.set(self.0);
    }
}

pub(crate) fn metadata(point: &str) -> io::Result<()> {
    hit(PROTOCOL.get(), point)
}

pub(crate) fn install(callback: Callback) -> io::Result<()> {
    CALLBACK
        .set(callback)
        .map_err(|_| io::Error::other("protocol hook already installed"))
}

pub(crate) fn hit(protocol: &str, point: &str) -> io::Result<()> {
    let Some(callback) = CALLBACK.get() else {
        return Ok(());
    };

    callback(&format!("{protocol}.{point}"))
}
