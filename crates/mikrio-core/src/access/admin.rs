//! Owned administration values. Preparing a key generates secret material
//! without persisting it or granting access.

use std::fmt;

use crate::EpochMillis;

use super::identity::APPLICATION_KEY_PREFIX;
use super::{CredentialClass, IdentityError, PrincipalId, Secret};

/// Immutable detached owner identity; neither field is an authentication secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerInfo {
    account_id: String,
    canonical_id: String,
}

impl OwnerInfo {
    /// Decode the store owner's two public encodings without normalizing them.
    ///
    /// # Errors
    /// Rejects noncanonical widths, casing, nondecimal accounts and nonhex IDs.
    pub fn new(account_id: String, canonical_id: String) -> Result<Self, IdentityError> {
        super::validate_account_id(&account_id)?;

        crate::values::decode_hex::<32>(canonical_id.as_bytes())
            .map_err(|_| IdentityError::CanonicalOwnerEncoding)?;

        Ok(Self {
            account_id,
            canonical_id,
        })
    }

    /// Exact public account identifier, including any leading zeroes.
    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Exact canonical-owner identifier used by S3 owner projections.
    #[must_use]
    pub fn canonical_id(&self) -> &str {
        &self.canonical_id
    }
}

/// Bounded human label, never an authorization identity or a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalName(String);

impl PrincipalName {
    /// Preserve exact UTF-8/case; no normalization or name-to-ARN rebinding.
    ///
    /// # Errors
    /// Rejects empty labels, control characters and more than 128 UTF-8 bytes.
    pub fn new(value: String) -> Result<Self, IdentityError> {
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(IdentityError::PrincipalName);
        }

        Ok(Self(value))
    }

    /// Original owned label; listing this value grants no authority.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A generated persistent credential identifier, separate from its secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessKeyId(String);

impl AccessKeyId {
    /// Validate an existing persistent ID; this does not import/provision a key.
    ///
    /// # Errors
    /// Rejects malformed IDs and every listener-bootstrap credential class.
    pub fn parse(value: String) -> Result<Self, IdentityError> {
        if CredentialClass::parse(&value)? != CredentialClass::Application {
            return Err(IdentityError::CredentialClass);
        }

        Ok(Self(value))
    }

    /// Canonical persistent token, safe to transport as UTF-8.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccessKeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Detached nonsecret application-principal metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalInfo {
    /// Stable random identity, independent of the human name.
    pub principal_id: PrincipalId,
    /// Exact bounded label.
    pub name: PrincipalName,
    /// False denies all persistent keys belonging to this principal.
    pub is_enabled: bool,
    /// Original creation time in the common checked millisecond range.
    pub created_at: EpochMillis,
}

/// Detached credential metadata suitable for listing; deliberately no secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessKeyInfo {
    /// Credential being listed/revoked.
    pub access_key_id: AccessKeyId,
    /// Stable identity owning this credential.
    pub principal_id: PrincipalId,
    /// Key creation time, not principal creation or last use.
    pub created_at: EpochMillis,
}

/// The result of committed key creation. No list or retry can recover its secret.
/// Construct this only after the creating transaction's outcome is established;
/// preparing random material is not a successful administration operation.
#[derive(Debug)]
pub struct CreateAccessKeyOutput {
    /// Detached nonsecret identity and creation metadata.
    pub access_key: AccessKeyInfo,
    /// Explicit one-time disclosure; Debug always remains redacted.
    pub secret_access_key: Secret,
}

/// A checked upper bound applied before allocating an administration page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminPageSize(u16);

impl Default for AdminPageSize {
    fn default() -> Self {
        Self(100)
    }
}

impl AdminPageSize {
    /// # Errors
    /// Rejects zero and values above 1,000 without narrowing or clamping.
    pub fn new(value: u32) -> Result<Self, IdentityError> {
        if !(1..=1000).contains(&value) {
            return Err(IdentityError::PageSize);
        }

        Ok(Self(
            u16::try_from(value).map_err(|_| IdentityError::PageSize)?,
        ))
    }

    #[must_use]
    pub fn get(self) -> u32 {
        u32::from(self.0)
    }
}

/// Detached live-keyset page; an empty collection is present, not absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPrincipalsOutput {
    /// Ascending binary principal-ID order, under the requested page bound.
    pub principals: Vec<PrincipalInfo>,
    /// Absent at the end; scoped to this store and operation, never a credential.
    pub next_continuation_token: Option<String>,
}

/// Nonsecret key inventory; it cannot identify or recover a lost creation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListAccessKeysOutput {
    /// Ascending access-key-ID order for the selected principal.
    pub access_keys: Vec<AccessKeyInfo>,
    /// Also bound to the principal; absence marks the end of the live list.
    pub next_continuation_token: Option<String>,
}

/// Exact validated policy text. No automatic Debug or serialization of grants.
pub struct GetPrincipalPolicyOutput {
    pub document: String,
}

/// Successful atomic policy replacement; no policy generation is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PutPrincipalPolicyOutput;

/// Successful policy removal, including an already absent policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletePrincipalPolicyOutput;

/// Successful revocation, including an already absent key of this principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteAccessKeyOutput;

/// Successful identity/policy deletion after explicit revocation of all its keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletePrincipalOutput;

/// Prepared random credential material, not a durable creation result.
/// The secret is disclosed once by the committed create result, never by listing.
#[derive(Debug)]
pub struct PreparedAccessKey {
    /// Public identifier and owner/time metadata for the SQL insert.
    pub info: AccessKeyInfo,
    /// Signing secret; Debug remains redacted at this enclosing boundary.
    pub secret_access_key: Secret,
}

impl PreparedAccessKey {
    /// Generate 128 random identity bits and 256 independent signing-secret bits.
    /// A SQL uniqueness collision must cause a fresh preparation, never reuse a
    /// revoked ID or substitute an existing key. No storage is changed here.
    ///
    /// # Errors
    /// Reports OS entropy failure with no time/PID/counter fallback.
    pub fn generate(
        principal_id: PrincipalId,
        created_at: EpochMillis,
    ) -> Result<Self, getrandom::Error> {
        Self::from_entropy(principal_id, created_at, getrandom::fill)
    }

    fn from_entropy(
        principal_id: PrincipalId,
        created_at: EpochMillis,
        fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Self, getrandom::Error> {
        let mut entropy = [0; 48];
        fill(&mut entropy)?;
        let (identifier, secret) = entropy.split_at(16);
        let mut access_key_id = String::from(APPLICATION_KEY_PREFIX);
        append_hex(&mut access_key_id, identifier);
        let mut secret_access_key = String::with_capacity(64);
        append_hex(&mut secret_access_key, secret);

        Ok(Self {
            info: AccessKeyInfo {
                access_key_id: AccessKeyId(access_key_id),
                principal_id,
                created_at,
            },
            secret_access_key: Secret(secret_access_key),
        })
    }
}

pub(super) fn append_hex(output: &mut String, bytes: &[u8]) {
    for byte in bytes {
        output.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 15)]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_key_results_redact_secrets_and_inventory_contains_only_public_facts() {
        let prepared = PreparedAccessKey::from_entropy(
            PrincipalId::from_bytes([1; 16]),
            EpochMillis::new(0).unwrap(),
            |bytes| {
                bytes.fill(0xcd);
                Ok(())
            },
        )
        .unwrap();

        let result = CreateAccessKeyOutput {
            access_key: prepared.info,
            secret_access_key: prepared.secret_access_key,
        };

        let inventory = ListAccessKeysOutput {
            access_keys: vec![result.access_key.clone()],
            next_continuation_token: None,
        };

        let secret = result.secret_access_key.expose();
        assert_eq!(secret.len(), 64);
        assert!(!format!("{result:?}").contains(secret));
        assert!(!format!("{inventory:?}").contains(secret));
        assert_eq!(inventory.access_keys[0], result.access_key);
    }

    #[test]
    fn administration_page_bounds_preserve_the_default_and_reject_overflow() {
        assert_eq!(AdminPageSize::default().get(), 100);

        for value in [1, 1000] {
            assert_eq!(AdminPageSize::new(value).unwrap().get(), value);
        }

        for value in [0, 1001, 65_537, u32::MAX] {
            assert_eq!(AdminPageSize::new(value), Err(IdentityError::PageSize));
        }
    }

    #[test]
    fn prepared_credentials_preserve_owner_and_time_without_disclosing_secret_in_debug() {
        let principal = PrincipalId::from_bytes([1; 16]);
        let created_at = EpochMillis::new(1_700_000_000_123).unwrap();

        let prepared = PreparedAccessKey::from_entropy(principal, created_at, |bytes| {
            bytes[..16].fill(0xab);
            bytes[16..].fill(0xcd);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            prepared.info.access_key_id.as_str(),
            "mikrio_aABABABABABABABABABABABABABABABAB"
        );

        assert_eq!(prepared.secret_access_key.expose(), "CD".repeat(32));
        assert_eq!(prepared.info.principal_id, principal);
        assert_eq!(prepared.info.created_at, created_at);
        assert!(!format!("{prepared:?}").contains("CDCD"));
        assert!(!format!("{:?}", prepared.info).contains("secret"));

        assert!(matches!(
            PreparedAccessKey::from_entropy(principal, created_at, |_| Err(
                getrandom::Error::UNSUPPORTED
            )),
            Err(getrandom::Error::UNSUPPORTED)
        ));
    }

    #[test]
    fn owner_and_principal_encodings_preserve_leading_zeroes_and_reject_aliases() {
        let owner = OwnerInfo::new("012345678901".into(), "ab".repeat(32)).unwrap();
        assert_eq!(owner.account_id(), "012345678901");
        assert_eq!(owner.canonical_id(), "ab".repeat(32));

        assert_eq!(
            OwnerInfo::new("12345678901".into(), "ab".repeat(32)),
            Err(IdentityError::AccountEncoding)
        );

        assert_eq!(
            OwnerInfo::new("012345678901".into(), "AB".repeat(32)),
            Err(IdentityError::CanonicalOwnerEncoding)
        );

        let principal = PrincipalId::parse("000102030405060708090a0b0c0d0e0f").unwrap();

        assert_eq!(
            principal.as_bytes(),
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );

        assert_eq!(
            principal.arn(owner.account_id()).unwrap(),
            "arn:aws:iam::012345678901:user/000102030405060708090a0b0c0d0e0f"
        );

        for invalid in [
            "app",
            "000102030405060708090A0B0C0D0E0F",
            "000102030405060708090a0b0c0d0e0f0",
        ] {
            assert_eq!(
                PrincipalId::parse(invalid),
                Err(IdentityError::PrincipalEncoding)
            );
        }
    }

    #[test]
    fn labels_count_utf8_bytes_without_rebinding_case_or_unicode_forms() {
        assert!(PrincipalName::new("é".repeat(64)).is_ok());

        assert_eq!(
            PrincipalName::new("é".repeat(65)),
            Err(IdentityError::PrincipalName)
        );

        for invalid in ["", "line\nfeed", "nul\0label"] {
            assert_eq!(
                PrincipalName::new(invalid.into()),
                Err(IdentityError::PrincipalName)
            );
        }

        assert_ne!(
            PrincipalName::new("é".into()).unwrap(),
            PrincipalName::new("e\u{301}".into()).unwrap()
        );

        assert_ne!(
            PrincipalName::new("App".into()).unwrap(),
            PrincipalName::new("app".into()).unwrap()
        );
    }
}
