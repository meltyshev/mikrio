//! Literal namespace grouping and bytewise seek boundaries shared by listings.

pub(super) fn common_prefix<'key>(
    key: &'key str,
    prefix: &str,
    delimiter: Option<&str>,
) -> Option<&'key str> {
    let delimiter = delimiter?;
    let position = key.get(prefix.len()..)?.find(delimiter)?;
    key.get(..prefix.len() + position + delimiter.len())
}

pub(super) fn successor(bytes: &[u8]) -> Option<Vec<u8>> {
    let index = bytes.iter().rposition(|byte| *byte != u8::MAX)?;
    let mut upper = bytes[..=index].to_vec();
    upper[index] += 1;
    Some(upper)
}
