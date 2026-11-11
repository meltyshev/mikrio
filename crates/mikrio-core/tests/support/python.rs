//! Select the fixture interpreter without relying on a Windows Store alias.

use std::process::Command;

pub(crate) fn command() -> Command {
    let executable = std::env::var_os("mikrio_test_python")
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());

    Command::new(executable)
}
