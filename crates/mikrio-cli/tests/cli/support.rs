//! Owned executable children and bounded observation of their actual output.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
#[cfg(unix)]
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

pub(super) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const DEADLINE: Duration = Duration::from_secs(20);

pub(super) fn command(root: &Path, arguments: &[&str]) -> Command {
    configured(Command::new(executable()), root, arguments)
}

#[cfg(unix)]
pub(super) fn descriptor_limited_command(root: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new("/bin/sh");

    command
        .args([
            "-c",
            "ulimit -n 32 || exit 1; exec \"$@\"",
            "cli-descriptor-limit",
        ])
        .arg("/usr/bin/env");

    // macOS strips loader variables from system executables. Restore Cargo's
    // existing test environment after the shell starts; installed qualification
    // supplies neither variable and still requires the adjacent companion.
    for name in ["DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
        if let Some(value) = std::env::var_os(name) {
            let mut assignment = std::ffi::OsString::from(name);
            assignment.push("=");
            assignment.push(value);
            command.arg(assignment);
        }
    }

    command.arg(executable());

    configured(command, root, arguments)
}

fn configured(mut command: Command, root: &Path, arguments: &[&str]) -> Command {
    command
        .current_dir(root)
        .args(arguments)
        .stdin(Stdio::null());

    for name in [
        "mikrio_host",
        "mikrio_port",
        "mikrio_public_endpoint",
        "mikrio_region",
        "mikrio_access_key_id",
        "mikrio_secret_access_key",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_PROFILE",
        "AWS_REGION",
    ] {
        command.env_remove(name);
    }

    command
}

pub(super) fn executable() -> std::ffi::OsString {
    std::env::var_os("MIKRIO_CLI_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mikrio").into())
}

pub(super) fn run(root: &Path, arguments: &[&str]) -> TestResult<Output> {
    Process::spawn(command(root, arguments))?.finish()
}

pub(super) fn success(output: &Output) -> TestResult<Value> {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.stderr.is_empty(),
        "unexpected successful-command diagnostic"
    );

    let value = one_line(&output.stdout)?;
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["event"], "result");
    Ok(value["result"].clone())
}

pub(super) fn error(output: &Output, exit: i32, phase: &str, code: &str) -> TestResult<Value> {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.stdout.is_empty(),
        "a failed operation emitted a result"
    );

    let value = one_line(&output.stderr)?;
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["event"], "error");
    assert_eq!(value["phase"], phase);
    assert_eq!(value["code"], code);
    Ok(value)
}

pub(super) fn one_line(bytes: &[u8]) -> TestResult<Value> {
    assert_eq!(bytes.split(|byte| *byte == b'\n').count(), 2);
    assert_eq!(bytes.last(), Some(&b'\n'));
    Ok(serde_json::from_slice(bytes)?)
}

pub(super) struct Process {
    child: Child,
    #[cfg(unix)]
    lines: Receiver<Vec<u8>>,
    #[cfg(unix)]
    diagnostics: Receiver<Vec<u8>>,
    stdout: Option<JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
}

impl Process {
    pub(super) fn spawn(command: Command) -> io::Result<Self> {
        Self::spawn_with_stdout(command, Stdio::piped())
    }

    #[cfg(unix)]
    pub(super) fn broken_stdout(command: Command) -> io::Result<Self> {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;

        let (writer, reader) = UnixStream::pair()?;
        drop(reader);
        Self::spawn_with_stdout(command, Stdio::from(OwnedFd::from(writer)))
    }

    fn spawn_with_stdout(mut command: Command, stdout: Stdio) -> io::Result<Self> {
        let mut child = command.stdout(stdout).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take();

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing stderr pipe"))?;

        #[cfg(unix)]
        let (sender, lines) = mpsc::channel();

        let stdout = std::thread::spawn(move || {
            let Some(stdout) = stdout else {
                return Ok(Vec::new());
            };

            let mut reader = BufReader::new(stdout.take(4 * 1024 * 1024));
            let mut bytes = Vec::new();

            loop {
                let mut line = Vec::new();

                if reader.read_until(b'\n', &mut line)? == 0 {
                    return Ok(bytes);
                }

                bytes.extend_from_slice(&line);
                #[cfg(unix)]
                let _delivery = sender.send(line);
            }
        });

        #[cfg(unix)]
        let (diagnostic_sender, diagnostics) = mpsc::channel();

        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut reader = BufReader::new(stderr.take(4 * 1024 * 1024));

            loop {
                let mut line = Vec::new();

                if reader.read_until(b'\n', &mut line)? == 0 {
                    break;
                }

                bytes.extend_from_slice(&line);
                #[cfg(unix)]
                let _delivery = diagnostic_sender.send(line);
            }

            Ok(bytes)
        });

        Ok(Self {
            child,
            #[cfg(unix)]
            lines,
            #[cfg(unix)]
            diagnostics,
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    #[cfg(unix)]
    pub(super) fn diagnostic(&self) -> TestResult<String> {
        Ok(String::from_utf8(self.diagnostics.recv_timeout(DEADLINE)?)?)
    }

    #[cfg(unix)]
    pub(super) fn ready(&self) -> TestResult<Value> {
        let record = one_line(&self.lines.recv_timeout(DEADLINE)?)?;
        assert_eq!(record["schemaVersion"], 1);
        assert_eq!(record["event"], "ready");
        Ok(record)
    }

    pub(super) fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut input = self
            .child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing input pipe"))?;

        input.write_all(bytes)
    }

    #[cfg(unix)]
    pub(super) fn signal(&self, signal: &str) -> io::Result<()> {
        let status = Command::new("/bin/kill")
            .args([signal, &self.child.id().to_string()])
            .status()?;

        if !status.success() {
            return Err(io::Error::other("could not signal owned CLI child"));
        }

        Ok(())
    }

    pub(super) fn finish(mut self) -> TestResult<Output> {
        let deadline = Instant::now() + DEADLINE;

        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }

            if Instant::now() >= deadline {
                return Err("CLI child did not settle within the test watchdog".into());
            }

            // Poll termination only; scenario ordering uses output/pipe handshakes.
            std::thread::sleep(Duration::from_millis(5));
        };

        let stdout = join(self.stdout.take())?;
        let stderr = join(self.stderr.take())?;

        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

fn join(reader: Option<JoinHandle<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    reader
        .ok_or_else(|| io::Error::other("missing output reader"))?
        .join()
        .map_err(|_| io::Error::other("output reader panicked"))?
}

impl Drop for Process {
    #[allow(
        clippy::print_stderr,
        reason = "owned-child cleanup failures must remain visible beside the primary failure"
    )]
    fn drop(&mut self) {
        let stopped = match self.child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => self
                .child
                .kill()
                .and_then(|()| self.child.wait().map(|_| ())),
            Err(error) => Err(error),
        };

        if let Err(error) = stopped {
            eprintln!("CLI test child cleanup failed: {error}");
        }

        for reader in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            if !matches!(reader.join(), Ok(Ok(_))) {
                eprintln!("CLI test output-reader cleanup failed");
            }
        }
    }
}
