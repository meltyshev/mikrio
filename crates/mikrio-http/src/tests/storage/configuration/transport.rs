//! Real HTTP/1 completion and rejected-request reuse against the owned listener.

use super::*;

#[derive(Debug, PartialEq)]
struct Snapshot {
    policy_generation: Option<Vec<u8>>,
    policy: Option<String>,
    flags: [Option<bool>; 4],
    tags: String,
    object_generation: Vec<u8>,
}

fn snapshot(fixture: &Fixture<'_>) -> rusqlite::Result<Snapshot> {
    let (tags, object_generation) = fixture.database.query_row(
        "SELECT tags_json,generation FROM objects WHERE key=?1",
        [b"tagged".as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    fixture.database.query_row(
        "SELECT policy_generation,policy_json,block_public_acls,ignore_public_acls,\
         block_public_policy,restrict_public_buckets FROM buckets WHERE name='photos'",
        [],
        |row| {
            Ok(Snapshot {
                policy_generation: row.get(0)?,
                policy: row.get(1)?,
                flags: [row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?],
                tags,
                object_generation,
            })
        },
    )
}

async fn headers(socket: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    let mut bytes = Vec::new();

    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(
            bytes.len() < 8192,
            "fixture response headers exceeded bound"
        );

        bytes.push(socket.read_u8().await?);
    }

    String::from_utf8(bytes).map_err(std::io::Error::other)
}

#[derive(Clone, Copy, Debug)]
enum Terminal {
    ShortLength,
    ShortChunk,
    MissingTerminator,
    InvalidChunkSize,
    InvalidTrailer,
    ChangedPayload,
    InvalidDocument,
}

impl Terminal {
    const ALL: [Self; 7] = [
        Self::ShortLength,
        Self::ShortChunk,
        Self::MissingTerminator,
        Self::InvalidChunkSize,
        Self::InvalidTrailer,
        Self::ChangedPayload,
        Self::InvalidDocument,
    ];
}

fn framed_request(
    method: &str,
    target: &str,
    prefix: &str,
    terminal: Terminal,
) -> (Vec<u8>, Vec<u8>) {
    let body = if matches!(terminal, Terminal::InvalidDocument) {
        format!("{prefix}!")
    } else {
        prefix.to_owned()
    };

    let mode = if matches!(
        terminal,
        Terminal::ShortLength | Terminal::ChangedPayload | Terminal::InvalidDocument
    ) {
        "header"
    } else {
        // Presigning admits an unsigned body before HTTP framing is consumed.
        // Header-authenticated unknown-length bodies require aws-chunked and
        // would correctly return 411 before reaching this malformed HTTP body.
        "query"
    };

    let raw = request_with_mode(method, target, &body, &[], mode);
    let raw = std::str::from_utf8(&raw).unwrap();
    let (head, _) = raw.split_once("\r\n\r\n").unwrap();

    let head = head.replace(
        "connection: close",
        "connection: keep-alive\r\nexpect: 100-continue",
    );

    let ending = match terminal {
        Terminal::InvalidDocument => {
            return (format!("{head}\r\n\r\n").into_bytes(), body.into_bytes());
        }
        Terminal::ChangedPayload | Terminal::ShortLength => {
            let head = head.replace(
                &format!("content-length: {}", body.len()),
                &format!("content-length: {}", body.len() + 1),
            );

            let body = if matches!(terminal, Terminal::ChangedPayload) {
                format!("{body}!")
            } else {
                body
            };

            return (format!("{head}\r\n\r\n").into_bytes(), body.into_bytes());
        }
        Terminal::ShortChunk => "2\r\nx",
        Terminal::MissingTerminator => "0\r\n",
        Terminal::InvalidChunkSize => "not-hex\r\n",
        Terminal::InvalidTrailer => "0\r\ninvalid trailer\r\n\r\n",
    };

    let head = head.replace(
        &format!("content-length: {}", body.len()),
        "transfer-encoding: chunked",
    );

    let mut framed = if body.is_empty() {
        String::new()
    } else {
        format!("{:x}\r\n{body}\r\n", body.len())
    };

    framed.push_str(ending);

    (format!("{head}\r\n\r\n").into_bytes(), framed.into_bytes())
}

async fn attempt(
    listener: &crate::Listener,
    head: &[u8],
    mut body: Vec<u8>,
    following: &[u8],
    terminal: Terminal,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut socket =
        tokio::net::TcpStream::connect(listener.endpoint().strip_prefix("http://").unwrap())
            .await?;

    socket.write_all(head).await?;
    // A real 100 response observes the adapter demanding this request's body.
    // A successful client write alone would only demonstrate kernel buffering.
    status(&headers(&mut socket).await?, 100);

    let truncated = matches!(
        terminal,
        Terminal::ShortLength | Terminal::ShortChunk | Terminal::MissingTerminator
    );

    if !truncated {
        body.extend_from_slice(following);
    }

    socket.write_all(&body).await?;

    if truncated {
        socket.shutdown().await?;
    }

    let mut response = Vec::new();
    let result = socket.take(32 * 1024).read_to_end(&mut response).await;

    if let Err(error) = result {
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
    }

    assert!(
        response.len() < 32 * 1024,
        "fixture response exceeded bound"
    );

    Ok(String::from_utf8(response)?)
}

#[test]
fn configuration_tcp_framing_and_reuse_preserve_predecessors() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16),policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/tagged",
                "payload",
                &[("x-amz-tagging", "visibility=private")],
            ))?,
            200,
        );

        fixture.runtime.block_on(async {
            for (operation, (method, target, prefix)) in [
                ("PUT", "/photos?policy", POLICY),
                ("PUT", "/photos?publicAccessBlock", BLOCK),
                ("PUT", "/photos?ownershipControls", ENFORCED),
                ("DELETE", "/photos?policy", ""),
                ("DELETE", "/photos?publicAccessBlock", ""),
                (
                    "PUT",
                    "/photos/tagged?tagging",
                    "<Tagging><TagSet/></Tagging>",
                ),
                ("GET", "/photos/tagged?tagging", ""),
                ("DELETE", "/photos/tagged?tagging", ""),
            ]
            .into_iter()
            .enumerate()
            {
                for (index, terminal) in Terminal::ALL.into_iter().enumerate() {
                    report_case(format_args!("operation={operation:?}, method={method:?}, target={target:?}, prefix={prefix:?}, index={index:?}, terminal={terminal:?}"));

                    fixture
                        .connection
                        .put_bucket_policy(bucket(), ORIGINAL.into())
                        .await?;

                    fixture
                        .connection
                        .put_public_access_block(
                            bucket(),
                            PublicAccessBlock {
                                block_public_acls: true,
                                ignore_public_acls: true,
                                block_public_policy: true,
                                restrict_public_buckets: true,
                            },
                        )
                        .await?;

                    let before = snapshot(fixture)?;

                    let bootstrap = Arc::new(Bootstrap::new(
                        "transport-fixture".into(),
                        mikrio_core::access::Secret::new(SECRET.into())?,
                    )?);

                    let listener =
                        crate::Listener::bind(&fixture.connection, "127.0.0.1", 0, None, bootstrap)
                            .await?;

                    let key = format!("following-{operation}-{index}");
                    let following = request("PUT", &format!("/photos/{key}"), "canary", &[]);
                    let (head, body) = framed_request(method, target, prefix, terminal);

                    let response = tokio::time::timeout(
                        Duration::from_secs(10),
                        attempt(&listener, &head, body, &following, terminal),
                    )
                    .await??;

                    listener.close().await?;

                    assert_eq!(snapshot(fixture)?, before, "{target} {terminal:?}");

                    let published: i64 = fixture.database.query_row(
                        "SELECT count(*) FROM objects WHERE key=?1",
                        [key.as_bytes()],
                        |row| row.get(0),
                    )?;

                    let reused = response.contains("HTTP/1.1 200 OK\r\n");
                    assert_eq!(published, i64::from(reused), "{target} {terminal:?}");

                    if reused {
                        // Only fully framed requests may be drained and reused.
                        // The canary is a distinct request, never a malformed tail.
                        assert!(
                            matches!(
                                terminal,
                                Terminal::ChangedPayload | Terminal::InvalidDocument
                            ),
                            "{target} {terminal:?}"
                        );

                        status(&response, 400);
                        assert_eq!(response.matches("HTTP/1.1 ").count(), 2);
                    } else if !response.is_empty() {
                        assert_eq!(
                            response.split_whitespace().nth(1),
                            Some("400"),
                            "{target} {terminal:?}: {:?}",
                            response.lines().next()
                        );
                    }

                    if method == "PUT" && matches!(terminal, Terminal::InvalidDocument) {
                        // Positive reuse control also proves the SQL key lookup.
                        assert!(reused, "{target} {terminal:?}");
                    }
                }
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        Ok(())
    })
}
