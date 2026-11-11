// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2023-2026 The s3s Authors

//! Part header block parsing and the header stage of the state machine.

use std::ops::Range;
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use futures_core::Stream;

use crate::Error;
use crate::multipart::{Multipart, State};

#[derive(Debug)]
pub struct HeaderBlock {
    // The already bounded header block limits this vector's size. Preserve all
    // fields so consumers can reject duplicates regardless of their position.
    spans: Vec<HeaderSpan>,
    next: usize,
    /// Offset of the first part data byte, always inside the block that
    /// `parse_header_block` was given. [`Multipart::finish_headers`] splits the
    /// buffer there, and `split_to` panics past the end instead of returning an
    /// error, so the offset has to stay within the parsed block.
    data_start: usize,
}

#[derive(Debug, Clone)]
struct HeaderSpan {
    name: Range<usize>,
    value: Range<usize>,
}

fn parse_header_block(block: &[u8]) -> Result<HeaderBlock, Error> {
    let capacity = memchr::memchr_iter(b'\n', block).count() + 1;
    let mut headers = vec![httparse::EMPTY_HEADER; capacity];
    match httparse::parse_headers(block, &mut headers) {
        Ok(httparse::Status::Complete((data_start, parsed))) => {
            let spans = parsed
                .iter()
                .map(|header| {
                    let name_start = header.name.as_ptr() as usize - block.as_ptr() as usize;
                    let value_start = header.value.as_ptr() as usize - block.as_ptr() as usize;
                    HeaderSpan {
                        name: name_start..name_start + header.name.len(),
                        value: value_start..value_start + header.value.len(),
                    }
                })
                .collect();
            Ok(HeaderBlock {
                spans,
                next: 0,
                data_start,
            })
        }
        Ok(httparse::Status::Partial) => Err(Error::IncompleteStream),
        Err(_) => Err(Error::InvalidFormat),
    }
}

impl<S> Multipart<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Send + Unpin,
{
    pub(super) fn next_header_inner(&mut self) -> Result<Option<httparse::Header<'_>>, Error> {
        let Some(block) = self.headers.as_mut() else {
            return Ok(None);
        };

        let Some(span) = block.spans.get(block.next).cloned() else {
            self.finish_headers();
            return Ok(None);
        };
        block.next = block.next.saturating_add(1);

        let buffer = self.buffer_mut()?;
        let name_bytes = buffer.buf.get(span.name.clone()).ok_or(Error::InvalidFormat)?;
        let value = buffer.buf.get(span.value.clone()).ok_or(Error::InvalidFormat)?;
        let name = std::str::from_utf8(name_bytes).map_err(|_| Error::InvalidFormat)?;
        Ok(Some(httparse::Header { name, value }))
    }

    pub(super) fn finish_headers(&mut self) {
        let Some(block) = self.headers.take() else {
            return;
        };
        if self.state == State::ReadingPartHeaders {
            let _ = self.buffer_mut().map(|buf| buf.buf.split_to(block.data_start));
            self.state = State::ReadingPartData;
        }
        // The part is in its data phase now, so it is no longer "handed out and
        // waiting": dropping it from here on cannot rewind the parser.
        self.part_handed_out = false;
    }

    pub(super) fn poll_ensure_headers(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if self.headers.is_some() || self.state == State::ReadingPartData {
            return Poll::Ready(Ok(()));
        }
        if self.state != State::ReadingPartHeaders {
            return Poll::Ready(Ok(()));
        }

        let max = self.max_buffer_size;
        let end = {
            let buffer = match self.buffer_mut() {
                Ok(buffer) => buffer,
                Err(err) => return Poll::Ready(Err(err)),
            };
            ready!(buffer.poll_read_header_block(max, cx))?
        };

        let block = match self.buffer_mut() {
            Ok(buffer) => parse_header_block(&buffer.buf[..end]),
            Err(err) => return Poll::Ready(Err(err)),
        };
        match block {
            Ok(block) => {
                self.headers = Some(block);
                Poll::Ready(Ok(()))
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unreachable, clippy::unwrap_used)]
mod tests {
    use super::*;

    use std::fmt::Write as _;

    use futures_util::stream;

    use crate::Boundary;

    fn parser_from_chunks(
        chunks: Vec<Result<&'static [u8], std::io::Error>>,
    ) -> Multipart<impl Stream<Item = Result<Bytes, Error>> + Send + Sync> {
        Multipart::new(
            stream::iter(
                chunks
                    .into_iter()
                    .map(|item| item.map(Bytes::from_static).map_err(Error::stream_read_failed)),
            ),
            &Boundary::new(b"boundary").unwrap(),
            4096,
        )
    }

    #[test]
    fn all_headers_are_validated_and_retained() {
        assert!(matches!(parse_header_block(b"X: y\r"), Err(Error::IncompleteStream)));
        assert!(matches!(parse_header_block(b"\0bad"), Err(Error::InvalidFormat)));
        let mut block = String::new();
        for index in 0..40 {
            let _ = write!(block, "X-{index}: value-{index}\r\n");
        }
        block.push_str("\r\n");
        let parsed = parse_header_block(block.as_bytes()).unwrap();
        assert_eq!(parsed.spans.len(), 40);
        assert_eq!(parsed.data_start, block.len());
        assert!(matches!(
            parse_header_block(b"X: 0\r\nY: 1\r\nZ: 2\r\nW: 3\r\nnot a header\r\n\r\n"),
            Err(Error::InvalidFormat)
        ));
    }

    #[test]
    fn header_offsets_come_from_the_parsed_block() {
        assert!(matches!(parse_header_block(b"X: y"), Err(Error::IncompleteStream)));
        assert!(matches!(parse_header_block(b"X: y\r\n"), Err(Error::IncompleteStream)));
        let parsed = parse_header_block(b"X:\r\n\r\n").unwrap();
        assert_eq!(parsed.spans.len(), 1);
        assert_eq!(parsed.spans[0].name, 0..1);
        assert_eq!(parsed.spans[0].value, 2..2);
        assert_eq!(parsed.data_start, 6);
        let parsed = parse_header_block(b"\r\n").unwrap();
        assert!(parsed.spans.is_empty());
        assert_eq!(parsed.data_start, 2);
    }

    /// The header stage must stay out of the way when the parser is already
    /// positioned somewhere else (moved here with `finish_headers`).
    #[test]
    fn finish_headers_keeps_non_header_state() {
        let mut mp = parser_from_chunks(vec![Ok(b"--boundary\r\nX: y\r\n\r\ndata\r\n--boundary--\r\n")]);
        mp.state = State::ReadingBoundary;
        mp.headers = Some(HeaderBlock {
            spans: Vec::new(),
            next: 0,
            data_start: 0,
        });
        mp.finish_headers();
        assert_eq!(mp.state, State::ReadingBoundary);
    }
}
