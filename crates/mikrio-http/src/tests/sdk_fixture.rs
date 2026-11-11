//! Owned, explicitly ignored subprocess entry for the installed SDK probe.

use std::future::Future as _;
use std::io::{BufRead as _, Write as _};
use std::pin::Pin;
use std::task::Poll;

use super::*;

#[test]
#[ignore = "Owned subprocess entry; invoked by tools.test_sdk, not a scenario"]
fn sdk_fixture_child() -> TestResult {
    Fixture::run(false, |fixture| {
        run_fixture(&fixture.runtime, &fixture.service, &fixture.observed)
    })
}

#[test]
#[ignore = "Owned subprocess entry; invoked by tools.test_sdk, not a scenario"]
fn phase_fixture_child() -> TestResult {
    let fixture = phases::fixture()?;
    run_fixture(&fixture.runtime, &fixture.service, &fixture.observed)
}

#[test]
#[ignore = "Owned subprocess entry; invoked by tools.test_sdk storage, not a scenario"]
fn storage_fixture_child() -> TestResult {
    use mikrio_core::object::{
        PutRequest, Representation, RepresentationInput, UserMetadata, WriteConditions,
    };

    storage::with_fixture(|fixture| {
        let target = storage::read("native-seed");

        fixture.runtime.block_on(fixture.connection.put_object(
            PutRequest {
                bucket: target.bucket,
                key: target.key,
                expected_owner: None,
                content_length: None,
                representation: Representation::new(RepresentationInput {
                    content_type: Some("text/plain".into()),
                    ..RepresentationInput::default()
                })?,
                metadata: UserMetadata::new([("title".into(), "café".into())])?,
                tags: None,
                integrity: mikrio_core::integrity::PutIntegrity::new(
                    mikrio_core::integrity::PutIntegrityInput::default(),
                )?,
                conditions: WriteConditions::default(),
            },
            b"native-to-sdk".as_slice(),
        ))?;

        serve_fixture(&fixture.runtime, &fixture.service)?;

        // Client assertions are supplemented by an independent native byte walk.
        // Both retained objects must still decode after closing and reopening core.
        fixture.runtime.block_on(fixture.connection.close())?;

        let reopened = fixture
            .cleanup
            .track(fixture.runtime.block_on(Connection::open(
                fixture.connection.info().directory.clone(),
                None,
            ))?);

        fixture.runtime.block_on(async {
            for (key, length, modulus) in [
                ("boto-shared", 257 * 1024, 251),
                ("node-shared", 263 * 1024, 239),
            ] {
                report_case(format_args!(
                    "key={key:?}, length={length:?}, modulus={modulus:?}"
                ));

                let (metadata, mut body) = reopened.get_object(storage::read(key)).await?;
                assert_eq!(metadata.length.get(), length);
                let mut offset = 0_u64;

                while let Some(chunk) = body.read_chunk(64 * 1024).await? {
                    for byte in chunk {
                        assert_eq!(
                            u64::from(byte),
                            offset % modulus,
                            "key={key:?}, offset={offset}"
                        );

                        offset += 1;
                    }
                }

                assert_eq!(offset, length);
                assert!(metadata.checksums.contains_key("CRC32"));
            }

            reopened.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })?;

    writeln!(
        std::io::stdout().lock(),
        "mikrio_observed {{\"reopened_objects\":2,\"verified_bytes\":532480}}"
    )?;

    Ok(())
}

fn run_fixture(
    runtime: &tokio::runtime::Runtime,
    service: &Arc<Service>,
    observed: &Mutex<Observations>,
) -> TestResult {
    serve_fixture(runtime, service)?;
    let observation = observed.lock().unwrap();

    let mut dispatch_controls = 0;
    let mut unexpected_dispatches = 0;

    for (_, key) in &observation.admitted {
        if !key.starts_with("dispatch-probe-") {
            continue;
        }

        if key == "dispatch-probe-control" {
            dispatch_controls += 1;
            continue;
        }

        unexpected_dispatches += 1;
    }

    writeln!(
        std::io::stdout().lock(),
        "mikrio_observed {{\"finishes\":{},\"bytes\":{},\"boto_crc32_header\":{},\"boto_crc32_trailer\":{},\"node_crc32_header\":{},\"node_crc32_trailer\":{},\"active_uploads\":{},\"peak_uploads\":{},\"unexpected_dispatches\":{},\"dispatch_controls\":{}}}",
        observation.finishes,
        observation.bytes,
        observation.boto_crc32.header,
        observation.boto_crc32.trailer,
        observation.node_crc32.header,
        observation.node_crc32.trailer,
        observation.active_uploads,
        observation.peak_uploads,
        unexpected_dispatches,
        dispatch_controls
    )?;

    Ok(())
}

fn serve_fixture(runtime: &tokio::runtime::Runtime, service: &Arc<Service>) -> TestResult {
    {
        let mut output = std::io::stdout().lock();
        writeln!(output, "mikrio_prepared")?;
        output.flush()?;
    }

    let (stop, stopped) = tokio::sync::oneshot::channel();

    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::stdin().lock().read_line(&mut line);
        // The watchdog may already have closed the receiver; this reader owns
        // no resource needed by the completed connection tasks.
        drop(stop.send(result.map(|_| line)));
    });

    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let mut output = std::io::stdout().lock();
        writeln!(output, "mikrio_ready http://{address}")?;
        output.flush()?;
        drop(output);

        let mut stopped = stopped;
        let deadline = tokio::time::sleep(Duration::from_secs(120));
        tokio::pin!(deadline);
        let mut connections = tokio::task::JoinSet::new();

        let mut outcome = loop {
            let event = std::future::poll_fn(|cx| {
                if let Poll::Ready(message) = Pin::new(&mut stopped).poll(cx) {
                    return Poll::Ready(Event::Stop(message));
                }

                if deadline.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Event::Timeout);
                }

                if !connections.is_empty()
                    && let Poll::Ready(Some(settled)) = connections.poll_join_next(cx)
                {
                    return Poll::Ready(Event::Settled(settled));
                }

                if connections.len() < 8 {
                    return listener
                        .poll_accept(cx)
                        .map(|value| Event::Accepted(value.map(|(socket, _)| socket)));
                }

                Poll::Pending
            })
            .await;

            match event {
                Event::Timeout => break Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
                Event::Stop(message) => {
                    break match message {
                        Ok(Ok(line)) if line == "stop\n" => Ok(()),
                        _ => Err(std::io::Error::other(
                            "fixture control channel closed unexpectedly",
                        )),
                    };
                }
                Event::Accepted(accepted) => {
                    let socket = match accepted {
                        Ok(socket) => socket,
                        Err(error) => break Err(error),
                    };

                    let service = Arc::clone(service);
                    connections.spawn(async move { service.serve(socket).await });
                }
                Event::Settled(settled) => {
                    if let Err(error) = settled {
                        break Err(std::io::Error::other(error));
                    }
                    // A peer may deliberately abandon a malformed request.
                    // Success is established by client outcomes and sink facts.
                }
            }
        };

        drop(listener);
        connections.abort_all();

        while let Some(settled) = connections.join_next().await {
            if let Err(error) = settled
                && !error.is_cancelled()
            {
                outcome = Err(std::io::Error::other(error));
            }
        }

        outcome
    });

    // The controlling runner always closes stdin before waiting/killing on an
    // error. A deadline failure exits this child without waiting on host input.
    if result.is_ok() {
        reader
            .join()
            .map_err(|_| "fixture control reader panicked")?;
    }

    result?;
    Ok(())
}

enum Event {
    Stop(Result<std::io::Result<String>, tokio::sync::oneshot::error::RecvError>),
    Timeout,
    Accepted(std::io::Result<tokio::net::TcpStream>),
    Settled(Result<std::io::Result<()>, tokio::task::JoinError>),
}
