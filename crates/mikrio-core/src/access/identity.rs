//! Credential classes and opaque HTTP identity. There is no native-authority
//! variant and no deserializer: request fields cannot manufacture a capability.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) const APPLICATION_KEY_PREFIX: &str = "mikrio_a";

/// A malformed identity value, distinct from a policy-language failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// Principal ID does not have the exact canonical 32 lowercase hex digits.
    PrincipalEncoding,
    /// Credential ID is outside the supported ASCII envelope or generated shape.
    CredentialEncoding,
    /// An otherwise valid credential belongs to the other lookup namespace.
    CredentialClass,
    /// Signing material is empty or exceeds its byte ceiling.
    SecretLength,
    /// Human label is empty, oversized, or contains a control character.
    PrincipalName,
    /// Store owner account is not exactly twelve ASCII decimal digits.
    AccountEncoding,
    /// Canonical owner ID does not have exactly 64 lowercase hex digits.
    CanonicalOwnerEncoding,
    /// Administration page size is outside its bounded 1-1,000 range.
    PageSize,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PrincipalEncoding => "invalid principal identifier",
            Self::CredentialEncoding => "invalid credential identifier",
            Self::CredentialClass => "credential belongs to another identity class",
            Self::SecretLength => "signing secret is outside the supported byte range",
            Self::PrincipalName => "invalid principal name",
            Self::AccountEncoding => "invalid owner account identifier",
            Self::CanonicalOwnerEncoding => "invalid canonical owner identifier",
            Self::PageSize => "administration page size must be between 1 and 1000",
        })
    }
}

impl std::error::Error for IdentityError {}

/// Validate an account identifier without normalization or allocation.
///
/// # Errors
/// Rejects any encoding other than exactly twelve ASCII decimal digits.
pub fn validate_account_id(value: &str) -> Result<(), IdentityError> {
    if value.len() != 12 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(IdentityError::AccountEncoding);
    }

    Ok(())
}

/// Opaque, immutable application identity; names and credential IDs are distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrincipalId([u8; 16]);

impl PrincipalId {
    /// Generate a new opaque identity from the operating system's random source.
    /// Storage checks uniqueness before returning a committed creation result.
    ///
    /// # Errors
    /// Reports entropy failure without a timestamp/counter fallback.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// Decode the canonical portable identifier, without resolving a human name.
    ///
    /// # Errors
    /// Rejects any other width, casing or encoding.
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        crate::values::decode_hex(value.as_bytes())
            .map(Self)
            .map_err(|_| IdentityError::PrincipalEncoding)
    }

    /// Decode exactly one persisted random identity.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Persisted BLOB representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Stable local ARN under the validated store account.
    ///
    /// # Errors
    /// Refuses an invalid twelve-digit owner-account identifier.
    pub fn arn(self, account: &str) -> Result<String, IdentityError> {
        validate_account_id(account)?;

        Ok(format!("arn:aws:iam::{account}:user/{self}"))
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

/// Owned signing material. Debug deliberately redacts it; no Serialize/Display.
#[derive(Clone)]
pub struct Secret(pub(super) String);

impl Secret {
    /// Validate the shared nonempty 1-1,024 UTF-8 byte storage envelope.
    ///
    /// # Errors
    /// Rejects an empty or oversized secret without including it in the error.
    pub fn new(value: String) -> Result<Self, IdentityError> {
        if value.is_empty() || value.len() > 1024 {
            return Err(IdentityError::SecretLength);
        }

        Ok(Self(value))
    }

    /// Deliberate disclosure to a signature implementation or initial key result.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

/// Disjoint lookup route selected before signature verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialClass {
    /// Only the persistent application-key table may resolve this ID.
    Application,
    /// Only this listener's configured bootstrap identity may resolve this ID.
    Bootstrap,
}

impl CredentialClass {
    /// Classify a bounded credential token. An unknown application key ID never falls back.
    ///
    /// # Errors
    /// Rejects malformed ASCII IDs and reserved-prefix encodings.
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        if value.is_empty() || value.len() > 128 {
            return Err(IdentityError::CredentialEncoding);
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
        {
            return Err(IdentityError::CredentialEncoding);
        }

        let Some(random_id) = value.strip_prefix(APPLICATION_KEY_PREFIX) else {
            return Ok(Self::Bootstrap);
        };

        if random_id.len() != 32 {
            return Err(IdentityError::CredentialEncoding);
        }

        if !random_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
        {
            return Err(IdentityError::CredentialEncoding);
        }

        Ok(Self::Application)
    }
}

pub(crate) enum Identity {
    Anonymous,
    Application { key: String, principal: PrincipalId },
    Bootstrap(Arc<AtomicBool>),
}

/// A private authenticated-path context. It never means trusted native access.
/// Application identity is rechecked against current SQL at mutation publication.
pub struct HttpIdentity(pub(crate) Identity);

impl HttpIdentity {
    /// Select anonymous access only when the adapter observed no credentials.
    /// Invalid supplied authentication must fail before calling this constructor.
    #[must_use]
    pub const fn anonymous() -> Self {
        Self(Identity::Anonymous)
    }
}

/// Signature verification input plus its opaque identity; neither is a grant.
pub struct SigningKey {
    pub(crate) secret: Secret,
    pub(crate) identity: Identity,
}

impl SigningKey {
    /// Borrow only for the signature implementation; never log this value.
    #[must_use]
    pub fn secret(&self) -> &Secret {
        &self.secret
    }

    /// Consume only after successful signature and admission-expiry validation.
    /// This transition grants no permission; current core authorization follows.
    /// The private adapter owns verification; decoded request values do not.
    #[must_use]
    pub fn verified(self) -> HttpIdentity {
        HttpIdentity(self.identity)
    }
}

/// Listener-local bootstrap authority. Close revokes existing identity contexts.
pub struct Bootstrap {
    key: String,
    secret: Secret,
    active: Arc<AtomicBool>,
}

impl Bootstrap {
    /// Generate independent listener-only identity and signing entropy.
    ///
    /// # Errors
    /// Reports operating-system entropy failure without fallback or persistence.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut entropy = [0; 48];
        getrandom::fill(&mut entropy)?;
        let mut key = String::from("mikrio_b");
        super::admin::append_hex(&mut key, &entropy[..16]);
        let mut secret = String::with_capacity(64);
        super::admin::append_hex(&mut secret, &entropy[16..]);

        Ok(Self {
            key,
            secret: Secret(secret),
            active: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Configure an already supplied listener credential pair without SQL writes.
    ///
    /// # Errors
    /// Refuses persistent-class IDs; there is no bootstrap/application conversion.
    pub fn new(key: String, secret: Secret) -> Result<Self, IdentityError> {
        if CredentialClass::parse(&key)? != CredentialClass::Bootstrap {
            return Err(IdentityError::CredentialClass);
        }

        Ok(Self {
            key,
            secret,
            active: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Detached connection details contain this nonsecret identifier.
    #[must_use]
    pub fn access_key_id(&self) -> &str {
        &self.key
    }

    /// Deliberate secret disclosure for the listener's detached credentials.
    #[must_use]
    pub fn secret(&self) -> &Secret {
        &self.secret
    }

    /// Resolve exactly this listener's bootstrap key; never consult app-key SQL.
    #[must_use]
    pub fn resolve(&self, key: &str) -> Option<SigningKey> {
        if key != self.key || !self.active.load(Ordering::Acquire) {
            return None;
        }

        Some(SigningKey {
            secret: self.secret.clone(),
            identity: Identity::Bootstrap(Arc::clone(&self.active)),
        })
    }

    /// Revoke this listener's authority without revoking another listener.
    pub fn close(&self) {
        self.active.store(false, Ordering::Release);
    }
}

impl Drop for Bootstrap {
    fn drop(&mut self) {
        self.close();
    }
}
