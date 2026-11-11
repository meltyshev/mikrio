#![no_main]

mod finite_stream;

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use libfuzzer_sys::fuzz_target;
use s3s::{MultipartLimits, transform_multipart};

type StreamError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, PartialEq, Eq)]
struct ParsedForm {
    fields: Vec<(String, String)>,
    payload: Vec<u8>,
}

fn parse(body: &[u8], chunk_size: usize, has_content_length: bool) -> Option<ParsedForm> {
    let chunks: Vec<Result<Bytes, StreamError>> = body
        .chunks(chunk_size)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();

    let limits = MultipartLimits {
        max_field_size: 20 * 1024,
        max_fields_size: 20 * 1024,
        max_parts: 64,
        max_envelope_size: Some(20 * 1024),
    };

    #[expect(
        clippy::expect_used,
        reason = "The target caps multipart bodies at 64 KiB, which fits u64."
    )]
    let content_length = has_content_length
        .then_some(u64::try_from(body.len()).expect("bounded multipart body length fits u64"));

    finite_stream::complete(async {
        let mut form =
            transform_multipart(stream::iter(chunks), b"mikrio-fuzz", limits, content_length)
                .await
                .ok()?;

        let fields = form.fields().to_vec();
        let mut file_stream = form.take_file_stream()?;
        let mut payload = Vec::new();

        while let Some(chunk) = file_stream.next().await {
            payload.extend_from_slice(&chunk.ok()?);
            assert!(payload.len() <= body.len(), "parser duplicated input bytes");
        }

        Some(ParsedForm { fields, payload })
    })
}

fuzz_target!(|input: &[u8]| {
    let Some((control, body)) = input.split_at_checked(3) else {
        return;
    };

    if body.len() > 65_536 {
        return;
    }

    let chunk_size = 1 + usize::from(u16::from_le_bytes([control[0], control[1]])) % 4096;
    let has_content_length = control[2] & 1 != 0;

    let contiguous = parse(body, body.len().max(1), has_content_length);
    let fragmented = parse(body, chunk_size, has_content_length);

    assert_eq!(
        contiguous, fragmented,
        "transport fragmentation changed the parse"
    );
});
