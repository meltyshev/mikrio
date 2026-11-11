//! Every administration mutation shares rollback and uncertain-commit semantics.

use super::*;

const DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;

#[derive(Clone, Copy, Debug)]
enum Mutation {
    CreatePrincipal,
    DisablePrincipal,
    DeletePrincipal,
    PutPolicy,
    DeletePolicy,
    CreateKey,
    DeleteKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrincipalRow {
    principal_id: Vec<u8>,
    name: String,
    is_enabled: i64,
    created_at_ms: i64,
    policy_generation: Option<Vec<u8>>,
    policy_json: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct KeyRow {
    access_key_id: String,
    principal_id: Vec<u8>,
    created_at_ms: i64,
}

#[derive(Debug, Eq, PartialEq)]
struct Snapshot {
    principals: Vec<PrincipalRow>,
    keys: Vec<KeyRow>,
}

fn snapshot(database: &MetadataDatabase) -> Result<Snapshot, rusqlite::Error> {
    let principals = database
        .connection
        .prepare(
            "SELECT principal_id, name, is_enabled, created_at_ms, policy_generation, policy_json
             FROM principals ORDER BY name",
        )?
        .query_map([], |row| {
            Ok(PrincipalRow {
                principal_id: row.get(0)?,
                name: row.get(1)?,
                is_enabled: row.get(2)?,
                created_at_ms: row.get(3)?,
                policy_generation: row.get(4)?,
                policy_json: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Never retain or print signing material in a comparison failure.
    let keys = database
        .connection
        .prepare(
            "SELECT access_key_id, principal_id, created_at_ms
             FROM access_keys ORDER BY access_key_id",
        )?
        .query_map([], |row| {
            Ok(KeyRow {
                access_key_id: row.get(0)?,
                principal_id: row.get(1)?,
                created_at_ms: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Snapshot { principals, keys })
}

impl Mutation {
    fn execute(
        self,
        database: &mut MetadataDatabase,
        principal: PrincipalId,
        key: Option<&AccessKeyId>,
    ) -> Result<(), OperationError> {
        match self {
            Self::CreatePrincipal => database
                .create_principal(PrincipalName::new("new-app".into()).unwrap())
                .map(|_| ()),
            Self::DisablePrincipal => database.set_principal_enabled(principal, false).map(|_| ()),
            Self::DeletePrincipal => database.delete_principal(principal),
            Self::PutPolicy => database.put_principal_policy(principal, DENY),
            Self::DeletePolicy => database.delete_principal_policy(principal),
            Self::CreateKey => database.create_access_key(principal).map(|_| ()),
            Self::DeleteKey => database.delete_access_key(principal, key.unwrap()),
        }
    }

    fn committed(self, before: &Snapshot, after: &Snapshot) -> TestResult {
        let mut expected = before.principals[0].clone();

        match self {
            Self::CreatePrincipal => {
                assert_eq!(after.principals.len(), 2);
                assert_eq!(after.principals[0], expected);
                let created = &after.principals[1];
                assert_eq!(created.name, "new-app");
                assert_eq!(created.principal_id.len(), 16);
                assert_ne!(created.principal_id, expected.principal_id);
                assert_eq!(created.is_enabled, 1);
                EpochMillis::new(created.created_at_ms)?;
                assert_eq!(created.policy_generation, None);
                assert_eq!(created.policy_json, None);
            }
            Self::DisablePrincipal => {
                expected.is_enabled = 0;
                assert_eq!(after.principals, [expected]);
            }
            Self::DeletePrincipal => assert_eq!(after.principals, []),
            Self::PutPolicy => {
                assert_eq!(after.principals.len(), 1);
                let generation = &after.principals[0].policy_generation;
                assert_ne!(*generation, expected.policy_generation);
                assert_eq!(generation.as_ref().map(Vec::len), Some(16));
                expected.policy_generation.clone_from(generation);
                expected.policy_json = Some(DENY.into());
                assert_eq!(after.principals, [expected]);
            }
            Self::DeletePolicy => {
                expected.policy_generation = None;
                expected.policy_json = None;
                assert_eq!(after.principals, [expected]);
            }
            Self::CreateKey | Self::DeleteKey => {
                assert_eq!(after.principals, before.principals);
            }
        }

        match self {
            Self::CreateKey => {
                assert_eq!(after.keys.len(), 1);
                let created = &after.keys[0];

                assert_eq!(
                    CredentialClass::parse(&created.access_key_id)?,
                    CredentialClass::Application
                );

                assert_eq!(created.principal_id, before.principals[0].principal_id);
                EpochMillis::new(created.created_at_ms)?;
            }
            Self::DeleteKey => assert_eq!(after.keys, []),
            _ => assert_eq!(after.keys, before.keys),
        }

        Ok(())
    }
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "identify each parameterized fault history"
)]
fn every_administration_mutation_preserves_rollback_and_lost_acknowledgment_states() -> TestResult {
    for mutation in [
        Mutation::CreatePrincipal,
        Mutation::DisablePrincipal,
        Mutation::DeletePrincipal,
        Mutation::PutPolicy,
        Mutation::DeletePolicy,
        Mutation::CreateKey,
        Mutation::DeleteKey,
    ] {
        for fault in [Fault::BeforeCommit, Fault::AfterCommit] {
            let directory = TestDirectory::new()?.retain_on_failure();
            let mut database = database(&directory)?;
            let owner = database.get_owner_info()?;
            let principal = database.create_principal(PrincipalName::new("app".into())?)?;
            let id = principal.principal_id;
            database.put_principal_policy(id, ALLOW)?;

            let key = if matches!(mutation, Mutation::DeleteKey) {
                Some(database.create_access_key(id)?.access_key.access_key_id)
            } else {
                None
            };

            let before = snapshot(&database)?;
            let committed = matches!(fault, Fault::AfterCommit);

            let outcome = if committed {
                MutationOutcome::Unknown
            } else {
                MutationOutcome::NotCommitted
            };

            println!("{mutation:?}: {outcome:?}");
            database.transaction_fault = Some(fault);

            let error = mutation
                .execute(&mut database, id, key.as_ref())
                .unwrap_err();

            assert_eq!(error.failure(), OperationFailure::IoError, "{mutation:?}");
            assert_eq!(error.outcome(), Some(outcome), "{mutation:?}");
            assert!(database.connection.is_autocommit());
            assert_eq!(database.get_owner_info()?, owner);
            let after = snapshot(&database)?;

            if committed {
                mutation.committed(&before, &after)?;
            } else {
                assert_eq!(after, before, "{mutation:?}");
            }

            close(database)?;

            let mut reopened = MetadataDatabase::open(
                &directory.path().join("metadata.sqlite3"),
                MetadataOpen::Existing,
            )?;

            assert_eq!(snapshot(&reopened)?, after, "{mutation:?}");
            assert_eq!(reopened.get_owner_info()?, owner);
            close(reopened)?;
            directory.finish()?;
        }
    }

    Ok(())
}
