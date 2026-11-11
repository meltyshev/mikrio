use super::*;
use crate::access::AdminPageSize;

fn seed_principal(database: &MetadataDatabase, value: u16) -> TestResult {
    let mut identity = [0_u8; 16];
    identity[14..].copy_from_slice(&value.to_be_bytes());

    database.connection.execute(
        "INSERT INTO principals (principal_id, name, is_enabled, created_at_ms)
         VALUES (?1, ?2, 1, 0)",
        params![identity.as_slice(), format!("page-{value}")],
    )?;

    Ok(())
}

fn labels(page: &crate::access::ListPrincipalsOutput) -> Vec<&str> {
    page.principals
        .iter()
        .map(|principal| principal.name.as_str())
        .collect()
}

fn seed_key(database: &MetadataDatabase, principal: PrincipalId, value: u16) -> TestResult {
    database.connection.execute(
        "INSERT INTO access_keys (access_key_id, principal_id, secret_access_key, created_at_ms)
         VALUES (?1, ?2, ?3, 0)",
        params![
            format!("mikrio_a{value:032X}"),
            principal.as_bytes().as_slice(),
            "owned-pagination-fixture-secret"
        ],
    )?;

    Ok(())
}

#[test]
fn live_key_pages_track_peer_mutations_across_restart() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("metadata.sqlite3");
    let mut database = database(&directory)?;

    let principal = database
        .create_principal(PrincipalName::new("app".into())?)?
        .principal_id;

    for value in [100, 200, 400] {
        seed_key(&database, principal, value)?;
    }

    let first = database.list_access_keys(principal, AdminPageSize::new(2)?, None)?;
    assert_eq!(first.access_keys.len(), 2);

    let token = first
        .next_continuation_token
        .ok_or("missing first key cursor")?;

    let mut peer = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
    peer.delete_access_key(principal, &first.access_keys[1].access_key_id)?;
    seed_key(&peer, principal, 150)?;
    seed_key(&peer, principal, 300)?;
    close(peer)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
    let second = reopened.list_access_keys(principal, AdminPageSize::new(1)?, Some(&token))?;
    assert_eq!(second.access_keys.len(), 1);

    assert_eq!(
        second.access_keys[0].access_key_id.as_str(),
        format!("mikrio_a{:032X}", 300)
    );

    assert!(!format!("{second:?}").contains("owned-pagination-fixture-secret"));

    let token = second
        .next_continuation_token
        .ok_or("missing second key cursor")?;

    let last = reopened.list_access_keys(principal, AdminPageSize::new(1)?, Some(&token))?;
    assert_eq!(last.access_keys.len(), 1);

    assert_eq!(
        last.access_keys[0].access_key_id.as_str(),
        format!("mikrio_a{:032X}", 400)
    );

    assert_eq!(last.next_continuation_token, None);
    let current = reopened.list_access_keys(principal, AdminPageSize::default(), None)?;

    let actual = current
        .access_keys
        .iter()
        .map(|key| key.access_key_id.as_str())
        .collect::<Vec<_>>();

    let expected = [100, 150, 300, 400].map(|value| format!("mikrio_a{value:032X}"));
    assert_eq!(actual, expected);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn live_principal_pages_track_peer_mutations_across_restart() -> TestResult {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("metadata.sqlite3");
    let mut database = database(&directory)?;

    for value in [100, 200, 400] {
        seed_principal(&database, value)?;
    }

    let first = database.list_principals(AdminPageSize::new(2)?, None)?;
    assert_eq!(labels(&first), ["page-100", "page-200"]);

    let token = first
        .next_continuation_token
        .ok_or("missing first cursor")?;

    let mut peer = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
    peer.delete_principal(first.principals[1].principal_id)?;
    seed_principal(&peer, 150)?;
    seed_principal(&peer, 300)?;
    close(peer)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
    let second = reopened.list_principals(AdminPageSize::new(1)?, Some(&token))?;
    assert_eq!(labels(&second), ["page-300"]);

    let token = second
        .next_continuation_token
        .ok_or("missing second cursor")?;

    let last = reopened.list_principals(AdminPageSize::new(1)?, Some(&token))?;
    assert_eq!(labels(&last), ["page-400"]);
    assert_eq!(last.next_continuation_token, None);
    let current = reopened.list_principals(AdminPageSize::default(), None)?;

    assert_eq!(
        labels(&current),
        ["page-100", "page-150", "page-300", "page-400"]
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn thousand_row_pages_validate_the_lookahead_before_returning_a_cursor() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    // Seed valid identities in one real transaction; randomness is not the
    // ordering oracle, and this fixture does not qualify public key generation.
    database.connection.execute_batch("BEGIN IMMEDIATE")?;

    for value in 1..=1000 {
        seed_principal(&database, value)?;
    }

    database.connection.execute_batch("COMMIT")?;

    let exact = database.list_principals(AdminPageSize::new(1000)?, None)?;
    assert_eq!(exact.principals.len(), 1000);
    assert_eq!(exact.next_continuation_token, None);
    seed_principal(&database, 1001)?;
    let first = database.list_principals(AdminPageSize::new(1000)?, None)?;
    assert_eq!(first.principals, exact.principals);

    let token = first
        .next_continuation_token
        .ok_or("missing lookahead cursor")?;

    let last = database.list_principals(AdminPageSize::new(1000)?, Some(&token))?;
    assert_eq!(labels(&last), ["page-1001"]);
    assert_eq!(last.next_continuation_token, None);

    database.connection.execute(
        "UPDATE principals SET name = ?1 WHERE name = 'page-1001'",
        ["invalid\nlookahead"],
    )?;

    assert_eq!(
        database
            .list_principals(AdminPageSize::new(1000)?, None)
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
