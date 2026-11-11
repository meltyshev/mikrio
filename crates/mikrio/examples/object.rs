//! Run with a fresh directory; retain the object so it can be inspected afterward.

use std::error::Error;
use std::io::Write;

use mikrio::{
    ChecksumAlgorithm, ChecksumMode, CreateBucketInput, GetObjectInput, OpenOptions,
    PutObjectInput, Store,
};

const PAYLOAD: &[u8] = b"Hello from mikrio.\n";

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("usage: object <dedicated-store-directory>")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(async {
        let store = Store::open(OpenOptions::new(directory)).await?;
        let result = round_trip(&store).await;

        // Always join store cleanup, including an upload/read/validation failure.
        let close = store.close().await;

        if let Err(error) = close {
            return Err(
                format!("store close failed: {error}; operation result: {result:?}").into(),
            );
        }

        result
    })
}

async fn round_trip(store: &Store) -> Result<(), Box<dyn Error>> {
    store
        .create_bucket(CreateBucketInput::builder("example").build()?)
        .await?;

    let written = store
        .put_object(
            PutObjectInput::builder("example", "hello.txt", PAYLOAD.to_vec())
                .content_type("text/plain; charset=utf-8")
                .checksum_algorithm(ChecksumAlgorithm::Sha256)
                .if_none_match("*")
                .build()?,
        )
        .await?;

    let mut selected = store
        .get_object(
            GetObjectInput::builder("example", "hello.txt")
                .if_match(written.etag())
                .checksum_mode(ChecksumMode::Enabled)
                .build()?,
        )
        .await?;

    let bytes = selected.body_mut().read_all(PAYLOAD.len()).await;
    let close = selected.body_mut().close().await;

    if let Err(error) = close {
        return Err(format!("body close failed: {error}; read result: {bytes:?}").into());
    }

    if bytes? != PAYLOAD || selected.checksum_sha256() != written.checksum_sha256() {
        return Err("readback differs from the acknowledged upload".into());
    }

    writeln!(
        std::io::stdout().lock(),
        "verified example/hello.txt {}",
        written.etag()
    )?;

    Ok(())
}
