/// One case-sensitive object tag. Values may be empty; keys must be unique.
/// The containing operation validates characters and UTF-16 length limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Exact tag key, without case folding or Unicode normalization.
    pub key: String,
    /// Exact required value; an empty string is distinct from an absent tag.
    pub value: String,
}
