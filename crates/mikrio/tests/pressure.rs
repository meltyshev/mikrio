//! External finite-volume/device failures through the ordinary facade artifact.

use std::io::{self, BufRead as _, Write as _};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use mikrio::{
    BodySource, CreateBucketInput, ErrorCode, GetObjectInput, MutationOutcome, OpenOptions,
    PutObjectInput, Store,
};
use tokio::io::{AsyncRead, ReadBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const OLD: &[u8] = &[0x41; 8192];
const BLOCK: &[u8] = &[0x42; 65536];

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PRESSURE {value}")?;
    output.flush()
}

fn control(expected: &str) -> io::Result<()> {
    let mut input = String::new();
    io::Read::take(io::stdin().lock(), 16).read_line(&mut input)?;

    if input != expected {
        return Err(io::Error::other("owned pressure control pipe changed"));
    }

    Ok(())
}

struct Source {
    remaining: usize,
    needs_start_signal: bool,
}

impl AsyncRead for Source {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.needs_start_signal {
            self.needs_start_signal = false;
            // Fixture-only host callback. The library has already reserved and
            // created staging before it asks for this first source chunk.
            announce("READY")?;
            control("run\n")?;
        }

        let length = self.remaining.min(output.remaining()).min(BLOCK.len());
        output.put_slice(&BLOCK[..length]);
        self.remaining -= length;
        Poll::Ready(Ok(()))
    }
}

#[test]
#[ignore = "Owned external Linux volume/device-pressure fixture; the linked libraries have no test hooks"]
fn ordinary_resource_pressure_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_pressure_directory").ok_or("missing owned store path")?,
    );

    let mode = std::env::var("mikrio_pressure_mode")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(async {
        let store = Store::open(OpenOptions::new(&path)).await?;

        match mode.as_str() {
            "prepare" => {
                store
                    .create_bucket(CreateBucketInput::builder("pressure").build()?)
                    .await?;

                store
                    .put_object(
                        PutObjectInput::builder("pressure", "object", OLD.to_vec())
                            .metadata(vec![("version".into(), "old".into())])
                            .build()?,
                    )
                    .await?;

                store.close().await?;
                announce("PREPARED")?;
            }
            "before" | "source" => {
                if mode == "before" {
                    announce("READY")?;
                    control("run\n")?;
                }

                let source = BodySource::from_reader(Source {
                    remaining: 16 * 1024 * 1024,
                    needs_start_signal: mode == "source",
                });

                let error = store
                    .put_object(
                        PutObjectInput::builder("pressure", "object", source)
                            .metadata(vec![("version".into(), "new".into())])
                            .build()?,
                    )
                    .await
                    .unwrap_err();

                assert_eq!(error.code(), ErrorCode::IoError);

                assert_eq!(
                    error.mutation_outcome(),
                    Some(&MutationOutcome::NotCommitted)
                );

                announce("RESULT IoError NotCommitted")?;
                // The external owner preserves the failing volume and kills
                // this process before restoring storage and reopening twice.
                control("exit\n")?;
                store.close().await?;
            }
            "verify" => {
                let mut selected = store
                    .get_object(GetObjectInput::builder("pressure", "object").build()?)
                    .await?;

                assert_eq!(selected.content_length(), 8192);

                assert_eq!(
                    selected.metadata().get("version").map(String::as_str),
                    Some("old")
                );

                assert_eq!(selected.body_mut().read_all(8192).await?, OLD);
                assert_eq!(selected.body_mut().read_chunk(1).await?, None);

                store
                    .put_object(
                        PutObjectInput::builder("pressure", "probe", b"writable".to_vec())
                            .build()?,
                    )
                    .await?;

                store.close().await?;
                announce("VERIFIED")?;
            }
            _ => return Err("unknown ordinary pressure mode".into()),
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
