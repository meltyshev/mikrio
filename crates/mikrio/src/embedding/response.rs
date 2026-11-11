//! Owned backend results and the existing bounded response cursor.
//!
//! Callers validate protocol facts before construction. A response reader must
//! own its transport work: dropping it cancels delivery without freeing memory
//! still used by pending I/O. Closing this cursor releases its response, not an
//! entire shared transport runtime or connection pool.

use std::collections::BTreeMap;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use mikrio_core::body::{BodyError, ReadBody, ReadSource};
use tokio::io::{AsyncRead, ReadBuf};

use crate::{
    BodySource, CommonPrefix, DeleteObjectOutput, ErrorCode, GetObjectOutput, HeadObjectOutput,
    ListObjectsV2Output, MutationOutcome, ObjectBody, ObjectSummary, PutObjectOutput, StorageError,
    Timestamp,
};

pub use crate::read_object::output::ReadMetadata as ReadFacts;

/// Detach a backend error and its authoritative mutation context.
#[must_use]
pub fn error(code: ErrorCode, outcome: Option<MutationOutcome>, cause: io::Error) -> StorageError {
    StorageError {
        code,
        cause: crate::ErrorCause::External(cause),
        mutation_outcome: outcome,
        partial_result: None,
    }
}

/// Construct acknowledged publication facts after protocol validation.
#[must_use]
pub fn put(etag: String, mut checksums: BTreeMap<String, String>) -> PutObjectOutput {
    PutObjectOutput {
        etag,
        checksum_crc32: checksums.remove("CRC32"),
        checksum_crc32c: checksums.remove("CRC32C"),
        checksum_crc64_nvme: checksums.remove("CRC64NVME"),
        checksum_sha1: checksums.remove("SHA1"),
        checksum_sha256: checksums.remove("SHA256"),
    }
}

/// Construct an acknowledged empty deletion result.
#[must_use]
pub const fn deleted() -> DeleteObjectOutput {
    DeleteObjectOutput
}

/// Transfer validated literal object facts without retaining a backend handle.
#[must_use]
pub const fn object_summary(
    key: String,
    size: u64,
    etag: String,
    last_modified: Timestamp,
) -> ObjectSummary {
    ObjectSummary {
        key,
        size,
        etag,
        last_modified,
    }
}

/// Transfer a validated bounded page. A next token means that it is truncated.
#[must_use]
pub fn listed(
    contents: Vec<ObjectSummary>,
    common_prefixes: Vec<String>,
    next_continuation_token: Option<String>,
) -> ListObjectsV2Output {
    ListObjectsV2Output {
        contents,
        common_prefixes: common_prefixes
            .into_iter()
            .map(|prefix| CommonPrefix { prefix })
            .collect(),
        is_truncated: next_continuation_token.is_some(),
        next_continuation_token,
    }
}

/// Resolve portable range syntax against a backend's reported full length.
///
/// # Errors
/// Returns the shared invalid syntax, length or unsatisfiable range error.
pub fn selected_content_range(range: &str, full_length: u64) -> Result<String, StorageError> {
    let range = mikrio_core::object::ByteRange::parse(range).map_err(StorageError::from_object)?;

    let length =
        mikrio_core::object::ByteLength::new(full_length).map_err(StorageError::from_object)?;

    let selected = range.select(length).map_err(StorageError::from_operation)?;
    Ok(selected.content_range(length))
}

/// Transfer validated detached HEAD facts.
#[must_use]
pub const fn head(details: ReadFacts) -> HeadObjectOutput {
    HeadObjectOutput { details }
}

/// Transfer validated GET facts and their uniquely owned response reader.
/// The existing cursor enforces bounded collection and terminal cancellation;
/// this source also rejects an early EOF or bytes beyond the selected length.
#[must_use]
pub fn get(details: ReadFacts, reader: BodySource) -> GetObjectOutput {
    let source = Response {
        reader: Some(reader),
        remaining: details.content_length,
    };

    GetObjectOutput {
        details,
        body: ObjectBody {
            body: ReadBody::new(source),
        },
    }
}

struct Response {
    reader: Option<BodySource>,
    remaining: u64,
}

impl ReadSource for Response {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), BodyError>> {
        let Some(reader) = &mut self.reader else {
            return Poll::Ready(Err(BodyError::closed()));
        };

        let before = buffer.filled().len();
        ready!(Pin::new(reader).poll_read(context, buffer))?;
        let count = u64::try_from(buffer.filled().len() - before).map_err(io::Error::other)?;

        if (count == 0 && self.remaining != 0) || count > self.remaining {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response length does not match its selected facts",
            )
            .into()));
        }

        self.remaining -= count;
        Poll::Ready(Ok(()))
    }

    fn cancel(mut self: Pin<&mut Self>) {
        self.reader = None;
    }

    fn poll_close(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.reader = None;
        Poll::Ready(Ok(()))
    }
}
