//! Descriptor preparation for owned children that hold every shard guard.

use std::io;

#[cfg(unix)]
pub(super) fn current() -> io::Result<(libc::rlim_t, libc::rlim_t)> {
    let limits = read()?;
    Ok((limits.rlim_cur, limits.rlim_max))
}

#[allow(
    unsafe_code,
    reason = "descriptor preparation applies only to owned fixture children"
)]
#[cfg(unix)]
pub(super) fn prepare() -> io::Result<serde_json::Value> {
    // All 256 shard guards coexist with SQLite, payload files, runtime handles
    // and control pipes. This is a fixture budget, not an engine limit.
    let mut limits = read()?;
    let soft_before = limits.rlim_cur;
    let hard_before = limits.rlim_max;

    if limits.rlim_cur < 512 {
        if limits.rlim_max < 512 {
            return Err(io::Error::other(
                "all-shard fixture requires a 512-descriptor child budget",
            ));
        }

        limits.rlim_cur = 512;

        // SAFETY: this owned child has not started its runtime. The OS reads
        // initialized stack storage; the hard limit and parent are unchanged.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limits) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    let after = read()?;

    if after.rlim_cur != soft_before.max(512) || after.rlim_max != hard_before {
        return Err(io::Error::other("unexpected fixture descriptor limits"));
    }

    Ok(serde_json::json!({
        "soft_before": soft_before,
        "soft_after": after.rlim_cur,
        "hard": hard_before,
        "scope": "owned all-shard fixture child only",
    }))
}

#[allow(
    unsafe_code,
    reason = "read native descriptor limits without changing the process"
)]
#[cfg(unix)]
fn read() -> io::Result<libc::rlimit> {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };

    // SAFETY: the OS writes only this initialized native rlimit, whose exclusive
    // stack storage remains valid throughout the synchronous call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limits) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(limits)
}

#[cfg(windows)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "share the fallible Unix fixture-preparation interface"
)]
pub(super) fn prepare() -> io::Result<serde_json::Value> {
    // Rust File uses kernel handles, not the CRT's adjustable descriptor table.
    // The fixture must still acquire all actual shard guards; no limit is raised.
    Ok(serde_json::json!({
        "rlimit_nofile": null,
        "scope": "Windows kernel handles; no Unix descriptor limit change",
    }))
}
