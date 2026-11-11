//! Indexed completed-part facts, validated only over the selected bounded window.

use rusqlite::{Row, Transaction, params};

use super::{Checksums, decode_error};
use crate::integrity::{Checksum, PartLayout};
use crate::multipart::PART_MAX_BYTES;
use crate::object::{SelectedPart, StoredObject, StoredPartPage};
use crate::{MetadataError, OperationError};

pub(in crate::metadata) const PAGE: &str =
    "SELECT part_number, byte_offset, byte_length, checksums_json, part_index
    FROM blob_parts WHERE blob_id=?1 AND part_number>?2 ORDER BY part_number LIMIT ?3";
const PREDECESSOR: &str = "SELECT part_number, byte_offset, byte_length, checksums_json, part_index
    FROM blob_parts WHERE blob_id=?1 AND part_number<=?2 ORDER BY part_number DESC LIMIT 1";
pub(in crate::metadata) const PART: &str =
    "SELECT part_number, byte_offset, byte_length, checksums_json, part_index
     FROM blob_parts WHERE blob_id=?1 AND part_index BETWEEN ?2 AND ?3
     ORDER BY part_index LIMIT 3";

/// Read at most the selected assembly position and its two neighbors.
pub(super) fn select_part(
    tx: &Transaction<'_>,
    object: &mut StoredObject,
    number: u32,
) -> Result<(), OperationError> {
    let count = object.part_count.unwrap_or(1);

    if number > count {
        return Err(OperationError::invalid_part_number(count, number));
    }

    let Some(count) = object.part_count else {
        if tx
            .prepare_cached("SELECT 1 FROM blob_parts WHERE blob_id=?1 LIMIT 1")?
            .exists([object.blob_id.as_bytes().as_slice()])?
        {
            return Err(corrupt().into());
        }

        object.part = Some(SelectedPart {
            offset: 0,
            length: object.length.get(),
        });

        return Ok(());
    };

    let first = number.saturating_sub(1).max(1);
    let last = (number + 1).min(count);
    let mut statement = tx.prepare_cached(PART)?;

    let mut rows = statement.query(params![
        object.blob_id.as_bytes().as_slice(),
        first,
        number + 1
    ])?;

    let mut previous: Option<PartLayout> = None;
    let mut selected = None;

    for index in first..=last {
        let row = rows.next()?.ok_or_else(corrupt)?;
        let (part, position) = decode(row, object)?;

        if position != index {
            return Err(corrupt().into());
        }

        if let Some(before) = &previous
            && (before.part_number >= part.part_number || end(before)? != part.byte_offset)
        {
            return Err(corrupt().into());
        }

        if index == number {
            selected = Some(part.clone());
        }

        previous = Some(part);
    }

    if rows.next()?.is_some() {
        return Err(corrupt().into());
    }

    let part = selected.ok_or_else(corrupt)?;

    object.part = Some(SelectedPart {
        offset: part.byte_offset,
        length: part.byte_length,
    });

    if object.checksum_type == Some(crate::integrity::ChecksumType::Composite) {
        let checksums: std::collections::BTreeMap<_, _> = part
            .checksums
            .into_iter()
            .map(|checksum| (checksum.algorithm().as_str().to_owned(), checksum.encode()))
            .collect();

        if !checksums.keys().eq(object.checksums.keys()) {
            return Err(corrupt().into());
        }

        object.checksums = checksums;
    } else if count != 1 {
        object.checksums.clear();
        object.checksum_type = None;
    }

    Ok(())
}

pub(in crate::metadata) fn page(
    tx: &Transaction<'_>,
    object: &StoredObject,
    marker: u32,
    limit: u32,
) -> Result<Option<StoredPartPage>, OperationError> {
    let mut statement = tx.prepare_cached(PAGE)?;

    let mut rows = statement.query(params![
        object.blob_id.as_bytes().as_slice(),
        marker,
        limit + 1
    ])?;

    let mut parts = Vec::new();

    while let Some(row) = rows.next()? {
        parts.push(decode(row, object)?);
    }

    let mut statement = tx.prepare_cached(PREDECESSOR)?;
    let mut rows = statement.query(params![object.blob_id.as_bytes().as_slice(), marker])?;
    let predecessor = rows.next()?.map(|row| decode(row, object)).transpose()?;

    let Some(count) = object.part_count else {
        if !parts.is_empty() || predecessor.is_some() {
            return Err(corrupt().into());
        }

        return Ok(None);
    };

    let observed = parts.len() + usize::from(predecessor.is_some());

    if observed == 0 || u32::try_from(observed).map_err(|_| corrupt())? > count {
        return Err(corrupt().into());
    }

    let mut previous = predecessor.as_ref();

    for entry in &parts {
        let (part, index) = entry;

        if let Some((before, before_index)) = previous {
            if before_index + 1 != *index || end(before)? != part.byte_offset {
                return Err(corrupt().into());
            }
        } else if *index != 1 {
            return Err(corrupt().into());
        }

        previous = Some(entry);
    }

    let truncated = u32::try_from(parts.len()).map_err(|_| corrupt())? > limit;

    if !truncated {
        let (last, index) = previous.ok_or_else(corrupt)?;

        if *index != count || end(last)? != object.length.get() {
            return Err(corrupt().into());
        }

        if predecessor.is_none() && u32::try_from(parts.len()).map_err(|_| corrupt())? != count {
            return Err(corrupt().into());
        }
    }

    let next_part_number_marker = if truncated {
        parts.pop();
        Some(parts.last().ok_or_else(corrupt)?.0.part_number)
    } else {
        None
    };

    Ok(Some(StoredPartPage {
        total_parts_count: count,
        entries_available: true,
        parts: parts.into_iter().map(|(part, _)| part).collect(),
        next_part_number_marker,
    }))
}

fn decode(row: &Row<'_>, object: &StoredObject) -> Result<(PartLayout, u32), MetadataError> {
    let number = row.get::<_, i64>(0).map_err(decode_error)?;
    let offset = row.get::<_, i64>(1).map_err(decode_error)?;
    let size = row.get::<_, i64>(2).map_err(decode_error)?;
    let encoded: String = row.get(3).map_err(decode_error)?;
    let index = row.get::<_, u32>(4).map_err(decode_error)?;
    let count = object.part_count.ok_or_else(corrupt)?;

    if !(1..=10_000).contains(&number) || encoded.len() > 4096 {
        return Err(corrupt());
    }

    let values: Checksums = serde_json::from_str(&encoded).map_err(|_| corrupt())?;

    let checksums = values
        .entries()
        .into_iter()
        .filter_map(|(algorithm, value)| {
            value.map(|value| Checksum::parse(algorithm, &value).map_err(|_| corrupt()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let part = PartLayout {
        part_number: u32::try_from(number).map_err(|_| corrupt())?,
        byte_offset: u64::try_from(offset).map_err(|_| corrupt())?,
        byte_length: u64::try_from(size).map_err(|_| corrupt())?,
        checksums,
    };

    if !(1..=count).contains(&index)
        || part.byte_length > PART_MAX_BYTES
        || end(&part)? > object.length.get()
        || (index == 1 && part.byte_offset != 0)
        || (index == count && end(&part)? != object.length.get())
        || (index < count && part.byte_length < 5 * 1024 * 1024)
    {
        return Err(corrupt());
    }

    if object.checksum_type == Some(crate::integrity::ChecksumType::Composite) {
        // Composite completion requires original numbers to equal assembly order.
        // A locally valid extent cannot make an impossible stored layout valid.
        if part.part_number != index {
            return Err(corrupt());
        }

        if part.checksums.len() != object.checksums.len() {
            return Err(corrupt());
        }

        if part
            .checksums
            .iter()
            .any(|checksum| !object.checksums.contains_key(checksum.algorithm().as_str()))
        {
            return Err(corrupt());
        }
    }

    Ok((part, index))
}

fn end(part: &PartLayout) -> Result<u64, MetadataError> {
    part.byte_offset
        .checked_add(part.byte_length)
        .ok_or_else(corrupt)
}

fn corrupt() -> MetadataError {
    MetadataError::Corrupt("invalid completed-object part layout")
}
