//! Requested live-allocation observations for isolated measurement children.
//!
//! Rust has no safe observer for its global allocator. This wrapper delegates
//! every operation unchanged to System; it measures requested bytes, excluding
//! allocator overhead, internal realloc copies, stack storage and OS cache.

#![allow(
    unsafe_code,
    reason = "test-only System allocator observer and SQLite memory status"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;

struct ObservedSystem;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: ObservedSystem = ObservedSystem;

fn retained(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every pointer/layout/size is forwarded unchanged to System, and only
// successful allocations affect counters. Atomics neither allocate nor unwind;
// the wrapper never reads or writes the pointed-to allocation itself.
unsafe impl GlobalAlloc for ObservedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc's caller supplies a valid nonzero Layout.
        let pointer = unsafe { System.alloc(layout) };

        if !pointer.is_null() {
            retained(layout.size());
        }

        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the same caller-provided Layout is delegated without changes.
        let pointer = unsafe { System.alloc_zeroed(layout) };

        if !pointer.is_null() {
            retained(layout.size());
        }

        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller guarantees this live pointer and matching Layout
        // came from this wrapper, which always allocated through System.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the live pointer, original Layout and valid new size retain
        // GlobalAlloc's provenance/alignment/lifetime contract unchanged.
        let resized = unsafe { System.realloc(pointer, layout, size) };

        if resized.is_null() {
            return resized;
        }

        if size >= layout.size() {
            retained(size - layout.size());
        } else {
            LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
        }

        resized
    }
}

fn sqlite(reset: bool) -> io::Result<(i64, i64)> {
    let mut current = 0;
    let mut highwater = 0;

    // SAFETY: both output pointers refer to distinct initialized i64 values
    // valid throughout the synchronous call. SQLite's process-global status
    // API is thread-safe; no SQLite pointer or allocation crosses this boundary.
    let result = unsafe {
        rusqlite::ffi::sqlite3_status64(
            rusqlite::ffi::SQLITE_STATUS_MEMORY_USED,
            &raw mut current,
            &raw mut highwater,
            i32::from(reset),
        )
    };

    if result != rusqlite::ffi::SQLITE_OK || current <= 0 || highwater < current {
        return Err(io::Error::other(
            "SQLite memory observations are unavailable",
        ));
    }

    Ok((current, highwater))
}

pub(crate) struct Sample {
    rust_baseline: usize,
    sqlite_baseline: i64,
}

#[derive(Serialize)]
pub(crate) struct Snapshot {
    rust_baseline: usize,
    rust_live: usize,
    rust_peak: usize,
    sqlite_baseline: i64,
    sqlite_live: i64,
    sqlite_peak: i64,
}

impl Sample {
    // Only the isolated child resets process-wide peaks, after opening SQLite.
    pub(crate) fn begin() -> io::Result<Self> {
        let (sqlite_baseline, _) = sqlite(true)?;
        let rust_baseline = LIVE.load(Ordering::Relaxed);
        PEAK.store(rust_baseline, Ordering::Relaxed);

        Ok(Self {
            rust_baseline,
            sqlite_baseline,
        })
    }

    pub(crate) fn finish(self) -> io::Result<Snapshot> {
        let rust_live = LIVE.load(Ordering::Relaxed);
        let rust_peak = PEAK.load(Ordering::Relaxed);
        let (sqlite_live, sqlite_peak) = sqlite(false)?;

        Ok(Snapshot {
            rust_baseline: self.rust_baseline,
            rust_live,
            rust_peak,
            sqlite_baseline: self.sqlite_baseline,
            sqlite_live,
            sqlite_peak,
        })
    }
}
