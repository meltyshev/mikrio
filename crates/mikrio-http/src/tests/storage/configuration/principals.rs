//! HTTP maps a nonexistent new policy principal without erasing historical state.

use mikrio_core::access::PrincipalName;

use super::*;

#[test]
fn deleted_bucket_policy_principal_is_malformed_on_apply_but_historical_policy_remains_readable()
-> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        let grantee = fixture.runtime.block_on(
            fixture
                .connection
                .create_principal(PrincipalName::new("grantee".into())?),
        )?;

        let owner = fixture
            .runtime
            .block_on(fixture.connection.get_owner_info())?;

        let arn = grantee.principal_id.arn(owner.account_id())?;

        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Principal":{{"AWS":"{arn}"}},"Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}}}"#
        );

        status(
            &fixture.exchange(&request("PUT", "/photos?policy", &document, &[]))?,
            204,
        );

        let before: (Vec<u8>, String) = fixture.database.query_row(
            "SELECT policy_generation, policy_json FROM buckets WHERE name = 'photos'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        fixture
            .runtime
            .block_on(fixture.connection.delete_principal(grantee.principal_id))?;

        let response = fixture.exchange(&request("GET", "/photos?policy", "", &[]))?;
        status(&response, 200);
        assert!(response.contains(&document));

        let response = fixture.exchange(&request("PUT", "/photos?policy", &document, &[]))?;
        status(&response, 400);
        assert!(response.contains("<Code>MalformedPolicy</Code>"));

        let after: (Vec<u8>, String) = fixture.database.query_row(
            "SELECT policy_generation, policy_json FROM buckets WHERE name = 'photos'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        assert_eq!(after, before);

        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.connection.get_bucket_policy(bucket()))?
                .document,
            document
        );

        Ok(())
    })
}
