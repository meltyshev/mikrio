// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2023-2026 The s3s Authors

use super::{FileStream, MultipartLimits, Parser, ParserError, ParserStream, bytes_to_string};
use crate::utils::SyncBoxFuture;

use s3s_multipart::parse_content_disposition_strict;
use transform_stream::AsyncTryStream;

impl FileStream {
    pub(super) fn with_tail(
        mut parser: Parser<ParserStream>,
        limits: MultipartLimits,
        mut part_count: usize,
        envelope_limit: usize,
    ) -> Self {
        let inner = AsyncTryStream::<_, _, SyncBoxFuture<'static, Result<(), ParserError>>>::new(|mut output| {
            Box::pin(async move {
                parser.set_consumed_limit(None)?;
                let mut file_size = 0_u64;
                {
                    let mut file = parser.resume_part()?;
                    while let Some(bytes) = file.next_data().await? {
                        file_size = file_size
                            .checked_add(bytes.len() as u64)
                            .ok_or(ParserError::EnvelopeTooLarge)?;
                        output.yield_ok(bytes).await;
                    }
                }

                let limit = file_size
                    .checked_add(envelope_limit as u64)
                    .ok_or(ParserError::EnvelopeTooLarge)?;
                parser.set_consumed_limit(Some(limit))?;
                while let Some(mut part) = parser.next_part().await? {
                    part_count = part_count.saturating_add(1);
                    if part_count > limits.max_parts {
                        return Err(ParserError::EnvelopeTooLarge);
                    }

                    let mut name = None;
                    while let Some(header) = part.next_header().await? {
                        if !header.name.eq_ignore_ascii_case("content-disposition") {
                            continue;
                        }
                        if name.is_some() {
                            return Err(ParserError::InvalidFormat);
                        }
                        let disposition = parse_content_disposition_strict(header.value).ok_or(ParserError::InvalidFormat)?;
                        if disposition.file_name.is_some() {
                            return Err(ParserError::StreamPartNotLast);
                        }
                        name = disposition
                            .name
                            .map(bytes_to_string)
                            .transpose()
                            .map_err(|_| ParserError::InvalidFormat)?;
                    }
                    part.consumed()?;
                    let name = name.ok_or(ParserError::InvalidFormat)?.to_ascii_lowercase();
                    if matches!(
                        name.as_str(),
                        "file" | "key" | "bucket" | "policy" | "signature" | "awsaccesskeyid"
                    ) || name.starts_with("x-amz-")
                    {
                        return Err(ParserError::StreamPartNotLast);
                    }

                    let mut field_size = 0_usize;
                    while let Some(bytes) = part.next_data().await? {
                        part.consumed()?;
                        field_size = field_size.saturating_add(bytes.len());
                        if field_size > limits.max_field_size {
                            return Err(ParserError::EnvelopeTooLarge);
                        }
                    }
                    part.consumed()?;
                }
                parser.consumed()?;
                Ok(())
            })
        });

        Self {
            inner: Box::pin(inner),
            content_len: None,
            remaining: 0,
            ended: false,
            length_range: (0, u64::MAX),
            yielded: 0,
        }
    }
}
