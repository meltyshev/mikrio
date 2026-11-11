//! Explicit public projections. Secrets are disclosed only by successful results.

use std::io::{self, Write};

use mikrio::{AccessKeyInfo, PrincipalInfo, Server, Timestamp};
use serde::Serialize;
use serde_json::{Value, json};

use crate::command::ResultValue;
use crate::diagnostics::Diagnostic;
use crate::failure::Failure;

pub(crate) struct Reporter<W, E> {
    stdout: W,
    stderr: E,
    json: bool,
    command: Option<&'static str>,
    exit: u8,
}

impl<W: Write, E: Write> Reporter<W, E> {
    pub(crate) const fn new(
        stdout: W,
        stderr: E,
        json: bool,
        command: Option<&'static str>,
    ) -> Self {
        Self {
            stdout,
            stderr,
            json,
            command,
            exit: 0,
        }
    }

    pub(crate) const fn exit(&self) -> u8 {
        self.exit
    }

    pub(crate) fn error(&mut self, failure: &Failure) {
        if self.exit != 1 {
            self.exit = failure.exit;
        }

        let written = if self.json {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct ErrorRecord<'a> {
                schema_version: u8,
                event: &'static str,
                #[serde(skip_serializing_if = "Option::is_none")]
                command: Option<&'static str>,
                phase: crate::failure::Phase,
                code: &'static str,
                message: &'a str,
                #[serde(skip_serializing_if = "Option::is_none")]
                mutation_outcome: Option<&'static str>,
            }

            write_json(
                &mut self.stderr,
                &ErrorRecord {
                    schema_version: 1,
                    event: "error",
                    command: self.command,
                    phase: failure.phase,
                    code: failure.code,
                    message: &failure.message,
                    mutation_outcome: failure.outcome.map(mikrio::MutationOutcome::as_str),
                },
            )
        } else {
            let command = self
                .command
                .map(|value| format!("{value}: "))
                .unwrap_or_default();

            let outcome = failure
                .outcome
                .map(|value| format!(" ({})", value.as_str()))
                .unwrap_or_default();

            writeln!(
                self.stderr,
                "mikrio: {command}{}: {}{}",
                failure.code,
                failure.message.trim_end(),
                outcome
            )
            .and_then(|()| self.stderr.flush())
        };

        if written.is_err() {
            // A broken diagnostic stream is itself an I/O failure; retrying it
            // recursively could conceal the original outcome or never finish.
            self.exit = 1;
        }
    }

    pub(crate) fn result(&mut self, result: &ResultValue) -> io::Result<()> {
        if !self.json
            && let ResultValue::Policy(document) = result
        {
            self.stdout.write_all(document.as_bytes())?;
            return self.stdout.flush();
        }

        let result = result_json(result)?;

        if self.json {
            return write_json(
                &mut self.stdout,
                &json!({
                    "schemaVersion": 1,
                    "event": "result",
                    "command": self.command,
                    "result": result,
                }),
            );
        }

        if result == json!({}) {
            writeln!(
                self.stdout,
                "{} completed",
                self.command.unwrap_or("command")
            )?;
        } else {
            serde_json::to_writer_pretty(&mut self.stdout, &result)?;
            writeln!(self.stdout)?;
        }

        if let Some(token) = result.get("nextContinuationToken").and_then(Value::as_str) {
            writeln!(
                self.stdout,
                "More results: pass --continuation-token {token}"
            )?;
        }

        self.stdout.flush()
    }

    pub(crate) fn diagnostic(&mut self, record: &Diagnostic) -> io::Result<()> {
        if self.json {
            return write_json(
                &mut self.stderr,
                &json!({
                    "schemaVersion": 1,
                    "event": "diagnostic",
                    "level": record.level(),
                    "message": record.message(),
                }),
            );
        }

        writeln!(
            self.stderr,
            "mikrio: {}: {}",
            record.level(),
            record.message()
        )?;

        self.stderr.flush()
    }

    pub(crate) fn ready(&mut self, server: &Server, generated_credentials: bool) -> io::Result<()> {
        if self.json {
            let mut record = json!({
                "schemaVersion": 1,
                "event": "ready",
                "endpoint": server.endpoint(),
                "region": server.region(),
            });

            if generated_credentials {
                record["credentials"] = json!({
                    "accessKeyId": server.credentials().access_key_id(),
                    "secretAccessKey": server.credentials().secret_access_key(),
                });
            }

            return write_json(&mut self.stdout, &record);
        }

        writeln!(
            self.stderr,
            "Endpoint: {}\nRegion: {}",
            server.endpoint(),
            server.region()
        )?;

        if generated_credentials {
            writeln!(
                self.stderr,
                "Access key ID: {}\nSecret access key: {}",
                server.credentials().access_key_id(),
                server.credentials().secret_access_key()
            )?;
        }

        self.stderr.flush()
    }
}

fn write_json(output: &mut impl Write, record: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    output.write_all(&bytes)?;
    output.flush()
}

fn result_json(result: &ResultValue) -> io::Result<Value> {
    match result {
        ResultValue::Init { store, owner } => Ok(json!({
            "storeId": store.store_id(),
            "directory": store.directory().to_str().ok_or_else(|| io::Error::other("non-UTF-8 store directory"))?,
            "formatVersion": store.format_version(),
            "region": store.region(),
            "accountId": owner.account_id(),
            "canonicalId": owner.canonical_id(),
        })),
        ResultValue::Principal {
            principal,
            account_id,
        } => principal_json(principal, account_id),
        ResultValue::Principals { page, account_id } => {
            let principals = page
                .principals()
                .iter()
                .map(|principal| principal_json(principal, account_id))
                .collect::<io::Result<Vec<_>>>()?;

            Ok(page_json(
                "principals",
                principals,
                page.next_continuation_token(),
            ))
        }
        ResultValue::AccessKey(output) => Ok(json!({
            "accessKey": key_json(output.access_key())?,
            "secretAccessKey": output.secret_access_key(),
        })),
        ResultValue::AccessKeys(page) => {
            let keys = page
                .access_keys()
                .iter()
                .map(key_json)
                .collect::<io::Result<Vec<_>>>()?;

            Ok(page_json(
                "accessKeys",
                keys,
                page.next_continuation_token(),
            ))
        }
        ResultValue::Policy(document) => Ok(json!({"document": document})),
        ResultValue::Empty => Ok(json!({})),
    }
}

fn principal_json(principal: &PrincipalInfo, account_id: &str) -> io::Result<Value> {
    Ok(json!({
        "principalId": principal.principal_id(),
        "name": principal.name(),
        "isEnabled": principal.is_enabled(),
        "createdAt": timestamp(principal.created_at())?,
        "arn": format!("arn:aws:iam::{account_id}:user/{}", principal.principal_id()),
    }))
}

fn key_json(key: &AccessKeyInfo) -> io::Result<Value> {
    Ok(json!({
        "accessKeyId": key.access_key_id(),
        "principalId": key.principal_id(),
        "createdAt": timestamp(key.created_at())?,
    }))
}

fn page_json(name: &str, values: Vec<Value>, token: Option<&str>) -> Value {
    let mut page = json!({});
    page[name] = Value::Array(values);

    if let Some(token) = token {
        page["nextContinuationToken"] = token.into();
    }

    page
}

fn timestamp(value: Timestamp) -> io::Result<String> {
    let instant = time::OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(value.as_unix_millis()) * 1_000_000,
    )
    .map_err(|_| io::Error::other("timestamp cannot be represented"))?;

    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        instant.year(),
        u8::from(instant.month()),
        instant.day(),
        instant.hour(),
        instant.minute(),
        instant.second(),
        instant.millisecond()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Phase;

    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn close_failure_retains_prior_result_and_committed_outcome() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new(), true, Some("policy apply"));
        reporter.result(&ResultValue::Empty).expect("result");
        reporter.error(&Failure::io(Phase::Close, "close failed").completed(true));
        assert_eq!(reporter.exit(), 1);
        let result: Value = serde_json::from_slice(&reporter.stdout).expect("result JSON");
        let failure: Value = serde_json::from_slice(&reporter.stderr).expect("error JSON");
        assert_eq!(result["result"], json!({}));
        assert_eq!(failure["phase"], "close");
        assert_eq!(failure["mutationOutcome"], "Committed");
    }

    #[test]
    fn secondary_close_failure_wins_exit_precedence_and_retains_both_errors() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new(), true, Some("policy apply"));
        reporter.error(&Failure::argument("invalid document"));
        reporter.error(&Failure::io(Phase::Close, "close failed"));
        reporter.error(&Failure::argument("another input failure"));
        assert_eq!(reporter.exit(), 1);

        let records = serde_json::Deserializer::from_slice(&reporter.stderr)
            .into_iter::<Value>()
            .collect::<Result<Vec<_>, _>>()
            .expect("diagnostics");

        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["message"], "invalid document");
        assert_eq!(records[1]["phase"], "close");
        assert!(records[1].get("mutationOutcome").is_none());
    }

    #[test]
    fn broken_diagnostic_stream_is_an_io_failure_without_recursive_output() {
        for json in [false, true] {
            let mut reporter = Reporter::new(Vec::new(), Broken, json, None);
            reporter.error(&Failure::argument("invalid input"));
            assert_eq!(reporter.exit(), 1);
            assert_eq!(reporter.stdout, Vec::<u8>::new());
        }
    }

    #[test]
    fn maintenance_diagnostics_preserve_results_and_report_output_failure() {
        let (mut diagnostics, dispatch) = crate::diagnostics::Diagnostics::new();

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(target: "mikrio", work = "cleanup", failure = "io", occurrences = 2_u64, "private canary");
        });

        let record = diagnostics.pending().expect("warning");

        for json_output in [false, true] {
            let mut reporter = Reporter::new(Vec::new(), Vec::new(), json_output, Some("init"));
            reporter.result(&ResultValue::Empty).expect("result");
            let result = reporter.stdout.clone();
            reporter.diagnostic(&record).expect("diagnostic");
            assert_eq!(reporter.exit(), 0);
            assert_eq!(reporter.stdout, result);
            assert!(!String::from_utf8_lossy(&reporter.stderr).contains("canary"));

            if json_output {
                let diagnostic: Value = serde_json::from_slice(&reporter.stderr).expect("JSON");
                assert_eq!(diagnostic["event"], "diagnostic");
                assert_eq!(diagnostic["level"], "warn");
            }

            let mut broken = Reporter::new(Vec::new(), Broken, json_output, None);
            assert!(broken.diagnostic(&record).is_err());
        }
    }
}
