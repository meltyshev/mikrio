//! Owned subprocesses and directories. Explicit cleanup reports errors; Drop is
//! a failure-path backstop and never removes a pre-existing directory.

use std::io::{self, BufRead, BufReader, Write};

use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static PROCESS_SCENARIO: Mutex<()> = Mutex::new(());

pub(crate) fn process_scenario() -> io::Result<MutexGuard<'static, ()>> {
    // A concurrently spawning test may transiently inherit another test's file
    // descriptors before exec closes them. Isolate scenarios, while retaining
    // each scenario's explicit real process/worker contention.
    PROCESS_SCENARIO
        .lock()
        .map_err(|_| io::Error::other("another process scenario failed"))
}

pub(crate) struct Probe {
    child: Child,
    observations: Receiver<io::Result<String>>,
    reader: Option<JoinHandle<()>>,
    reaped: bool,
    context: String,
    started: Instant,
}

impl Probe {
    pub(crate) fn start(directory: &Path, mode: &str, entry: &str) -> io::Result<Self> {
        Self::start_configured(directory, mode, entry, |_| Ok(()))
    }

    pub(crate) fn start_configured(
        directory: &Path,
        mode: &str,
        entry: &str,
        configure: impl FnOnce(&mut Command) -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);

        // Serial libtest output otherwise prefixes the first handshake with
        // `test <name> ... `, so it no longer begins with `PROBE `.
        command
            .args([
                "--ignored",
                "--exact",
                entry,
                "--nocapture",
                "--format=terse",
            ])
            .env("mikrio_probe_directory", directory)
            .env("mikrio_probe_mode", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        configure(&mut command)?;
        let started = Instant::now();
        let mut child = command.spawn()?;

        let context = format!(
            "child={} entry={entry} mode={mode} directory={}",
            child.id(),
            directory.display()
        );

        trace(&format!("spawned {context}"));
        let stdout = child.stdout.take();
        let (sender, observations) = mpsc::sync_channel(4);

        let mut probe = Self {
            child,
            observations,
            reader: None,
            reaped: false,
            context,
            started,
        };

        let reader = std::thread::Builder::new()
            .name("native-probe-output".into())
            .spawn(move || {
                let Some(stdout) = stdout else { return };

                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) => {
                            if let Some(observation) = line.strip_prefix("PROBE ")
                                && sender.try_send(Ok(observation.to_owned())).is_err()
                            {
                                break;
                            }
                        }
                        Err(error) => {
                            let _sent = sender.try_send(Err(error));
                            break;
                        }
                    }
                }
            })?;

        probe.reader = Some(reader);
        Ok(probe)
    }

    #[allow(
        dead_code,
        reason = "synchronous guard scenarios share support with async listener tests"
    )]
    pub(crate) fn observation(&self) -> io::Result<String> {
        let result = self
            .observations
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| self.observation_error(error))?;

        self.record_observation(&result);
        result
    }

    #[allow(
        dead_code,
        reason = "async storage scenarios share this support with synchronous guard tests"
    )]
    pub(crate) async fn observation_async(&self) -> io::Result<String> {
        // A peer may need a guard owned by a task on the caller's runtime. Keep
        // driving that runtime; neither recv_timeout nor a blocking-pool waiter
        // is safe when the owner needs that same execution capacity to settle.
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match self.observations.try_recv() {
                    Ok(observation) => return observation,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::other("probe output disconnected"));
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
        .await
        .map_err(|error| self.observation_error(error))?;

        self.record_observation(&result);
        result
    }

    fn observation_error(&self, error: impl std::fmt::Display) -> io::Error {
        let message = format!(
            "{} observation deadline=10s elapsed_since_spawn={:?}: {error}",
            self.context,
            self.started.elapsed()
        );

        trace(&message);
        io::Error::other(message)
    }

    fn record_observation(&self, result: &io::Result<String>) {
        trace(&format!(
            "{} observed={result:?} elapsed_since_spawn={:?}",
            self.context,
            self.started.elapsed()
        ));
    }

    #[allow(
        dead_code,
        reason = "synchronous guard scenarios share support with async listener tests"
    )]
    pub(crate) fn finish(&mut self) -> io::Result<()> {
        self.release()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);

        loop {
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                self.join_reader()?;

                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other("probe failed"))
                };
            }

            if std::time::Instant::now() >= deadline {
                return Err(io::Error::other("probe shutdown timed out"));
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[allow(
        dead_code,
        reason = "async storage scenarios share this support with synchronous guard tests"
    )]
    pub(crate) async fn finish_async(&mut self) -> io::Result<()> {
        self.release()?;

        if self.wait_async().await?.success() {
            Ok(())
        } else {
            Err(io::Error::other("probe failed"))
        }
    }

    #[allow(
        dead_code,
        reason = "async storage scenarios share this support with synchronous guard tests"
    )]
    pub(crate) async fn kill_and_wait_async(&mut self) -> io::Result<()> {
        self.child.kill()?;
        self.wait_async().await?;
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "multi-phase storage probes share guard-only support"
    )]
    pub(crate) fn command(&mut self, command: &str) -> io::Result<()> {
        let input = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("missing child input"))?;

        writeln!(input, "{command}")?;
        input.flush()
    }

    fn release(&mut self) -> io::Result<()> {
        self.child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing child input"))?
            .write_all(b"exit\n")
    }

    async fn wait_async(&mut self) -> io::Result<ExitStatus> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    self.reaped = true;
                    self.join_reader()?;
                    return Ok(status);
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(crate) fn kill_and_wait(&mut self) -> io::Result<()> {
        self.child.kill()?;
        let status = self.child.wait()?;

        trace(&format!(
            "{} reaped={status} elapsed_since_spawn={:?}",
            self.context,
            self.started.elapsed()
        ));

        self.reaped = true;
        self.join_reader()
    }

    fn join_reader(&mut self) -> io::Result<()> {
        if let Some(reader) = self.reader.take() {
            reader
                .join()
                .map_err(|_| io::Error::other("probe output reader panicked"))?;
        }

        Ok(())
    }
}

#[allow(
    dead_code,
    reason = "shared support also serves guard-only test binaries"
)]
#[allow(
    clippy::print_stderr,
    reason = "retained test diagnostics identify child progress and watchdogs"
)]
pub(crate) fn trace(stage: &str) {
    let time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    eprintln!(
        "PROBE_TRACE pid={} time_ms={time_ms} {stage}",
        std::process::id()
    );
}

impl Drop for Probe {
    fn drop(&mut self) {
        if !self.reaped {
            report_cleanup(self.kill_and_wait());
        }
    }
}

#[allow(
    clippy::print_stderr,
    reason = "test cleanup failures must remain visible beside the primary failure"
)]
fn report_cleanup(result: io::Result<()>) {
    if let Err(error) = result {
        eprintln!("test resource cleanup failed: {error}");
    }
}
