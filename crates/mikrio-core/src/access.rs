//! Bounded access-policy evaluation, separate from signature verification.
//! Decisions require current security and resource facts from the caller.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value};

mod admin;
mod condition;
mod configuration;
mod identity;
mod json;
mod pattern;

pub use admin::{
    AccessKeyId, AccessKeyInfo, AdminPageSize, CreateAccessKeyOutput, DeleteAccessKeyOutput,
    DeletePrincipalOutput, DeletePrincipalPolicyOutput, GetPrincipalPolicyOutput,
    ListAccessKeysOutput, ListPrincipalsOutput, OwnerInfo, PreparedAccessKey, PrincipalInfo,
    PrincipalName, PutPrincipalPolicyOutput,
};
use condition::Condition;
pub use configuration::{BucketAccessRequest, GetBucketPolicyOutput};
pub(crate) use identity::Identity;
pub use identity::{
    Bootstrap, CredentialClass, HttpIdentity, IdentityError, PrincipalId, Secret, SigningKey,
    validate_account_id,
};

/// A bounded invalid input; arbitrary policy text never enters its display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// The complete input exceeds the shared 20 KiB UTF-8 document limit.
    TooLarge,
    /// Malformed JSON, duplicate members, wrong types or missing required fields.
    Invalid,
    /// A policy element/condition/action lies outside the qualified subset.
    Unsupported,
    /// A bucket policy refers to another bucket or nonlocal principal.
    WrongScope,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "policy exceeds the document byte limit",
            Self::Invalid => "invalid policy document or context",
            Self::Unsupported => "unsupported policy semantics",
            Self::WrongScope => "policy resource or principal is outside its scope",
        })
    }
}

impl std::error::Error for PolicyError {}

/// The attachment owns principal and resource scope, never a JSON field.
#[derive(Clone, Copy)]
pub enum PolicyKind<'a> {
    /// Principal is implicit; a Principal member is forbidden.
    Identity,
    /// Resource and exact principal ARNs must belong to this local bucket/account.
    Bucket {
        /// Validated bucket name supplied by the attachment owner.
        name: &'a str,
        /// Persisted twelve-digit account identity.
        owner_account_id: &'a str,
    },
}

const ACTIONS: &[&str] = &[
    "s3:CreateBucket",
    "s3:ListAllMyBuckets",
    "s3:ListBucket",
    "s3:DeleteBucket",
    "s3:GetBucketLocation",
    "s3:PutBucketCORS",
    "s3:GetBucketCORS",
    "s3:PutLifecycleConfiguration",
    "s3:GetLifecycleConfiguration",
    "s3:PutObject",
    "s3:GetObject",
    "s3:DeleteObject",
    "s3:GetObjectAttributes",
    "s3:ListMultipartUploadParts",
    "s3:ListBucketMultipartUploads",
    "s3:AbortMultipartUpload",
    "s3:PutBucketPolicy",
    "s3:GetBucketPolicy",
    "s3:DeleteBucketPolicy",
    "s3:GetBucketPolicyStatus",
    "s3:PutBucketPublicAccessBlock",
    "s3:GetBucketPublicAccessBlock",
    "s3:PutBucketOwnershipControls",
    "s3:GetBucketOwnershipControls",
    "s3:GetBucketAcl",
    "s3:GetObjectAcl",
    "s3:PutObjectTagging",
    "s3:GetObjectTagging",
    "s3:DeleteObjectTagging",
];

/// One explicit permission check, not an HTTP operation-name transformation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action(&'static str);

impl Action {
    /// Resolve a supported action using IAM's ASCII case-insensitive action rule.
    ///
    /// # Errors
    /// Rejects unknown actions and patterns; patterns belong only in policies.
    pub fn parse(value: &str) -> Result<Self, PolicyError> {
        ACTIONS
            .iter()
            .find(|action| action.eq_ignore_ascii_case(value))
            .copied()
            .map(Self)
            .ok_or(PolicyError::Unsupported)
    }

    /// Canonical S3 permission name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    fn public_read(self) -> bool {
        matches!(self.0, "s3:GetObject" | "s3:GetObjectAttributes")
    }

    pub(crate) fn object_resource(self) -> bool {
        matches!(
            self.0,
            "s3:PutObject"
                | "s3:GetObject"
                | "s3:DeleteObject"
                | "s3:GetObjectAttributes"
                | "s3:ListMultipartUploadParts"
                | "s3:AbortMultipartUpload"
                | "s3:GetObjectAcl"
                | "s3:PutObjectTagging"
                | "s3:GetObjectTagging"
                | "s3:DeleteObjectTagging"
        )
    }
}

/// Explicit absence differs from a present empty string or present empty set.
/// Values are operation-specific facts supplied by the core/HTTP interpretation.
#[derive(Clone, Default)]
pub struct Context(BTreeMap<String, Vec<String>>);

impl Context {
    /// Insert a recognized context key without collapsing duplicates/case aliases.
    ///
    /// # Errors
    /// Rejects unsupported keys, duplicate aliases, excess members or value bytes.
    /// Tag-key suffixes use provisional ASCII case-insensitive matching.
    pub fn insert(&mut self, key: &str, values: Vec<String>) -> Result<(), PolicyError> {
        let key = condition::key(key)?;

        if values.len() > 10 {
            return Err(PolicyError::Invalid);
        }

        if values.iter().any(|value| value.len() > 1024) {
            return Err(PolicyError::Invalid);
        }

        if key != "s3:requestobjecttagkeys" && values.len() != 1 {
            return Err(PolicyError::Invalid);
        }

        if self.0.contains_key(&key) || self.0.len() >= 26 {
            return Err(PolicyError::Invalid);
        }

        if key == "s3:objectcreationoperation" && !matches!(values[0].as_str(), "true" | "false") {
            return Err(PolicyError::Invalid);
        }

        self.0.insert(key, values);
        Ok(())
    }
}

/// A policy's result; an implicit denial may be satisfied by another attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No applicable statement grants this action/resource/context.
    ImplicitDeny,
    /// At least one applicable allow and no explicit denial.
    Allow,
    /// An applicable denial, overriding every other attachment's allow.
    ExplicitDeny,
}

enum Principal {
    Attached,
    All,
    Exact(Vec<PrincipalArn>),
}

struct PrincipalArn {
    arn: String,
    principal_id: PrincipalId,
}

struct Statement {
    allow: bool,
    actions: Vec<Action>,
    resources: Vec<String>,
    principal: Principal,
    conditions: Vec<Condition>,
}

/// Validated bounded input plus its transient evaluation representation.
/// Deliberately does not implement Debug/Serialize: documents may contain keys.
pub struct Policy {
    document: String,
    statements: Vec<Statement>,
    public: bool,
    bucket: Option<String>,
}

impl Policy {
    /// Check document bytes, JSON depth and duplicate members before queuing work.
    /// Bucket/principal scope and supported statements still require [`Self::parse`]
    /// with the store's current owner identity.
    ///
    /// # Errors
    /// Rejects oversized, malformed or ambiguous JSON without logging its contents.
    pub fn validate_envelope(document: &str) -> Result<(), PolicyError> {
        json::parse(document).map(drop)
    }

    /// Validate the entire document, including late statements, before mutation.
    /// Preserve the exact input for storage instead of reserializing an AST.
    ///
    /// # Errors
    /// Rejects malformed/unsupported input and resource/principal scope violations.
    pub fn parse(document: &str, kind: PolicyKind<'_>) -> Result<Self, PolicyError> {
        if let PolicyKind::Bucket {
            name,
            owner_account_id,
        } = kind
        {
            if name.is_empty() || name.len() > 63 {
                return Err(PolicyError::WrongScope);
            }

            if owner_account_id.len() != 12 {
                return Err(PolicyError::WrongScope);
            }

            if !owner_account_id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(PolicyError::WrongScope);
            }
        }

        let root = json::parse(document)?;
        let root = object(&root, &["Version", "Id", "Statement"])?;

        if root.get("Version").and_then(Value::as_str) != Some("2012-10-17") {
            return Err(PolicyError::Unsupported);
        }

        optional_text(root, "Id")?;

        let values = match root.get("Statement") {
            Some(Value::Object(value)) => vec![value],
            Some(Value::Array(values)) if !values.is_empty() => values
                .iter()
                .map(|value| value.as_object().ok_or(PolicyError::Invalid))
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(PolicyError::Invalid),
        };

        let statements = values
            .into_iter()
            .map(|value| statement(value, kind))
            .collect::<Result<Vec<_>, _>>()?;

        let public = statements
            .iter()
            .any(|statement| statement.allow && matches!(statement.principal, Principal::All));

        let bucket = match kind {
            PolicyKind::Identity => None,
            PolicyKind::Bucket { name, .. } => Some(format!("arn:aws:s3:::{name}")),
        };

        Ok(Self {
            document: document.into(),
            statements,
            public,
            bucket,
        })
    }

    /// Original validated UTF-8 document, suitable for bounded TEXT storage.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    /// Classification of supported grants, independent of effective BPA denial.
    #[must_use]
    pub const fn is_public(&self) -> bool {
        self.public
    }

    /// Distinct local identities in first-reference order for a new bucket document.
    /// Historical documents remain parseable after a referenced identity is deleted.
    pub(crate) fn referenced_principals(&self) -> impl Iterator<Item = PrincipalId> + '_ {
        let mut seen = BTreeSet::new();

        self.statements
            .iter()
            .flat_map(|statement| match &statement.principal {
                Principal::Exact(values) => values.as_slice(),
                Principal::Attached | Principal::All => &[],
            })
            .map(|principal| principal.principal_id)
            .filter(move |principal| seen.insert(*principal.as_bytes()))
    }

    /// Evaluate one permission/resource using explicit operation context.
    /// Principal is the authenticated local ARN, never a user label/access-key ID.
    ///
    /// # Errors
    /// Rejects unbounded or malformed resource/principal inputs. Authentication,
    /// identity liveness, owner assertions and attachment selection belong to the
    /// enclosing current-state authorization decision, not this policy function.
    pub fn evaluate(
        &self,
        action: Action,
        resource: &str,
        principal_arn: Option<&str>,
        context: &Context,
    ) -> Result<Verdict, PolicyError> {
        if resource.len() > 1100 {
            return Err(PolicyError::Invalid);
        }

        if resource != "*" && !resource.starts_with("arn:aws:s3:::") {
            return Err(PolicyError::Invalid);
        }

        if principal_arn.is_some_and(|principal| principal.len() > 128) {
            return Err(PolicyError::Invalid);
        }

        if self.bucket.as_ref().is_some_and(|bucket| {
            if resource == bucket {
                return false;
            }

            !resource
                .strip_prefix(bucket)
                .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            return Ok(Verdict::ImplicitDeny);
        }

        let mut verdict = Verdict::ImplicitDeny;

        for statement in &self.statements {
            if !statement.actions.contains(&action) {
                continue;
            }

            if !statement
                .resources
                .iter()
                .any(|value| pattern::matches(value, resource))
            {
                continue;
            }

            let principal_matches = match &statement.principal {
                Principal::Attached => principal_arn.is_some(),
                Principal::All => true,
                Principal::Exact(values) => principal_arn
                    .is_some_and(|principal| values.iter().any(|value| value.arn == principal)),
            };

            if !principal_matches {
                continue;
            }

            if !statement
                .conditions
                .iter()
                .all(|condition| condition.matches(context))
            {
                continue;
            }

            if !statement.allow {
                return Ok(Verdict::ExplicitDeny);
            }

            verdict = Verdict::Allow;
        }

        Ok(verdict)
    }
}

fn statement(value: &Map<String, Value>, kind: PolicyKind<'_>) -> Result<Statement, PolicyError> {
    known_members(
        value,
        &[
            "Sid",
            "Effect",
            "Action",
            "Resource",
            "Principal",
            "Condition",
        ],
    )?;

    optional_text(value, "Sid")?;

    let allow = match value.get("Effect").and_then(Value::as_str) {
        Some("Allow") => true,
        Some("Deny") => false,
        _ => return Err(PolicyError::Invalid),
    };

    // Expand into the fixed permission vocabulary rather than accumulating one
    // allocation per repeated wildcard in an otherwise bounded document.
    let mut selected = [false; ACTIONS.len()];

    for candidate in strings(value.get("Action").ok_or(PolicyError::Invalid)?)? {
        let normalized = candidate.to_ascii_lowercase();
        let mut matched = false;

        for (selected, action) in selected.iter_mut().zip(ACTIONS) {
            if pattern::matches(&normalized, &action.to_ascii_lowercase()) {
                *selected = true;
                matched = true;
            }
        }

        if !matched {
            return Err(PolicyError::Unsupported);
        }
    }

    let actions: Vec<_> = selected
        .into_iter()
        .zip(ACTIONS)
        .filter_map(|(selected, action)| selected.then_some(Action(action)))
        .collect();

    let resources = strings(value.get("Resource").ok_or(PolicyError::Invalid)?)?;

    for resource in &resources {
        validate_resource(resource, kind)?;
    }

    let principal = principal(value.get("Principal"), kind)?;

    if allow
        && matches!(principal, Principal::All)
        && actions.iter().any(|action| !action.public_read())
    {
        return Err(PolicyError::Unsupported);
    }

    let conditions = value
        .get("Condition")
        .map(Condition::parse)
        .transpose()?
        .unwrap_or_default();

    Ok(Statement {
        allow,
        actions,
        resources,
        principal,
        conditions,
    })
}

fn principal(value: Option<&Value>, kind: PolicyKind<'_>) -> Result<Principal, PolicyError> {
    let PolicyKind::Bucket {
        owner_account_id, ..
    } = kind
    else {
        return if value.is_none() {
            Ok(Principal::Attached)
        } else {
            Err(PolicyError::Unsupported)
        };
    };

    if owner_account_id.len() != 12 || !owner_account_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PolicyError::WrongScope);
    }

    let value = value.ok_or(PolicyError::Invalid)?;

    if value.as_str() == Some("*") {
        return Ok(Principal::All);
    }

    let members = object(value, &["AWS"])?;
    let values = strings(members.get("AWS").ok_or(PolicyError::Invalid)?)?;

    if values == ["*"] {
        return Ok(Principal::All);
    }

    let prefix = format!("arn:aws:iam::{owner_account_id}:user/");
    let mut principals = Vec::with_capacity(values.len());

    for value in values {
        let Some(identity) = value.strip_prefix(&prefix) else {
            return Err(PolicyError::WrongScope);
        };

        let principal_id = PrincipalId::parse(identity).map_err(|_| PolicyError::WrongScope)?;

        principals.push(PrincipalArn {
            arn: value,
            principal_id,
        });
    }

    Ok(Principal::Exact(principals))
}

fn validate_resource(resource: &str, kind: PolicyKind<'_>) -> Result<(), PolicyError> {
    if resource.contains("${") {
        return Err(PolicyError::Unsupported);
    }

    if resource == "*" {
        return Ok(());
    }

    let rest = resource
        .strip_prefix("arn:aws:s3:::")
        .ok_or(PolicyError::Unsupported)?;

    if rest.is_empty() {
        return Err(PolicyError::Invalid);
    }

    let PolicyKind::Bucket { name, .. } = kind else {
        return Ok(());
    };

    if rest == name {
        return Ok(());
    }

    if !rest
        .strip_prefix(name)
        .is_some_and(|suffix| suffix.starts_with('/'))
    {
        return Err(PolicyError::WrongScope);
    }

    Ok(())
}

fn strings(value: &Value) -> Result<Vec<String>, PolicyError> {
    let values = match value {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) if !values.is_empty() => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(PolicyError::Invalid)
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(PolicyError::Invalid),
    };

    if values.iter().any(String::is_empty) {
        return Err(PolicyError::Invalid);
    }

    Ok(values)
}

fn object<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, PolicyError> {
    let object = value.as_object().ok_or(PolicyError::Invalid)?;
    known_members(object, allowed)?;
    Ok(object)
}

fn known_members(value: &Map<String, Value>, allowed: &[&str]) -> Result<(), PolicyError> {
    if value.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(PolicyError::Unsupported);
    }

    Ok(())
}

fn optional_text(value: &Map<String, Value>, field: &str) -> Result<(), PolicyError> {
    if value.get(field).is_some_and(|value| !value.is_string()) {
        return Err(PolicyError::Invalid);
    }

    Ok(())
}

#[cfg(test)]
mod tests;
