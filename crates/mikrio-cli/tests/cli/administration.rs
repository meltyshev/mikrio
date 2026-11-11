use std::process::Stdio;

use super::*;

pub(super) const IDENTITY_POLICY: &str = "{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": {\"Effect\":\"Allow\",\"Action\":[\"s3:GetObject\",\"s3:PutObject\"],\"Resource\":\"arn:aws:s3:::uploads/allowed-*\"}\n}\n";

#[test]
fn identity_and_key_lifecycle_has_exact_public_results_and_owned_revocation() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();
    let store = success(&run(root, &["init", "store", "--json"])?)?;

    let principal = success(&run(
        root,
        &["principal", "create", "store", "--name", "app", "--json"],
    )?)?;

    let principal_id = field(&principal, "principalId")?;
    assert_eq!(principal.as_object().ok_or("principal object")?.len(), 5);
    assert_eq!(principal["name"], "app");
    assert_eq!(principal["isEnabled"], true);

    assert_eq!(
        principal["arn"],
        format!(
            "arn:aws:iam::{}:user/{principal_id}",
            field(&store, "accountId")?
        )
    );

    let created_at = field(&principal, "createdAt")?;
    assert_eq!(created_at.len(), 24);
    assert!(created_at.ends_with('Z'));
    assert_eq!(created_at.as_bytes()[19], b'.');

    assert_eq!(
        principal,
        success(&run(
            root,
            &[
                "principal",
                "get",
                "store",
                "--principal-id",
                principal_id,
                "--json"
            ]
        )?)?
    );

    error(
        &run(
            root,
            &["principal", "create", "store", "--name=app", "--json"],
        )?,
        1,
        "operation",
        "PrincipalAlreadyExists",
    )?;

    let key = success(&run(
        root,
        &[
            "access-key",
            "create",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    assert_eq!(key.as_object().ok_or("key object")?.len(), 2);
    assert_eq!(field(&key, "secretAccessKey")?.len(), 64);
    assert_eq!(key["accessKey"]["principalId"], principal_id);
    let key_id = field(&key["accessKey"], "accessKeyId")?;

    let inventory = success(&run(
        root,
        &[
            "access-key",
            "list",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    assert_eq!(inventory, json!({"accessKeys": [key["accessKey"]]}));

    assert!(
        !inventory
            .to_string()
            .contains(field(&key, "secretAccessKey")?)
    );

    for (verb, expected) in [("disable", false), ("enable", true)] {
        let state = success(&run(
            root,
            &[
                "principal",
                verb,
                "store",
                "--principal-id",
                principal_id,
                "--json",
            ],
        )?)?;

        assert_eq!(state["isEnabled"], expected);
        assert_eq!(state["createdAt"], principal["createdAt"]);
        assert_eq!(state["arn"], principal["arn"]);
    }

    error(
        &run(
            root,
            &[
                "principal",
                "delete",
                "store",
                "--principal-id",
                principal_id,
                "--json",
            ],
        )?,
        1,
        "operation",
        "PrincipalHasAccessKeys",
    )?;

    let other = success(&run(
        root,
        &["principal", "create", "store", "--name=other", "--json"],
    )?)?;

    error(
        &run(
            root,
            &[
                "access-key",
                "revoke",
                "store",
                "--principal-id",
                field(&other, "principalId")?,
                "--access-key-id",
                key_id,
                "--json",
            ],
        )?,
        1,
        "operation",
        "InvalidArgument",
    )?;

    for _ in 0..2 {
        assert_eq!(
            success(&run(
                root,
                &[
                    "access-key",
                    "revoke",
                    "store",
                    "--principal-id",
                    principal_id,
                    "--access-key-id",
                    key_id,
                    "--json"
                ]
            )?)?,
            json!({})
        );
    }

    success(&run(
        root,
        &[
            "principal",
            "delete",
            "store",
            "--principal-id",
            principal_id,
            "--json",
        ],
    )?)?;

    error(
        &run(
            root,
            &[
                "principal",
                "get",
                "store",
                "--principal-id",
                principal_id,
                "--json",
            ],
        )?,
        1,
        "operation",
        "NoSuchPrincipal",
    )?;

    let replacement = success(&run(
        root,
        &["principal", "create", "store", "--name=app", "--json"],
    )?)?;

    assert_ne!(replacement["principalId"], principal_id);
    assert_ne!(replacement["arn"], principal["arn"]);
    directory.finish()?;
    Ok(())
}

#[test]
fn inventory_is_one_bounded_page_with_scoped_tokens_and_no_secrets() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();
    success(&run(root, &["init", "store", "--json"])?)?;

    let first = success(&run(
        root,
        &["principal", "create", "store", "--name=one", "--json"],
    )?)?;

    let second = success(&run(
        root,
        &["principal", "create", "store", "--name=two", "--json"],
    )?)?;

    let page = success(&run(
        root,
        &["principal", "list", "store", "--max-results=1", "--json"],
    )?)?;

    assert_eq!(
        page["principals"].as_array().ok_or("principal page")?.len(),
        1
    );

    let token = field(&page, "nextContinuationToken")?;

    let next = success(&run(
        root,
        &[
            "principal",
            "list",
            "store",
            "--max-results=1",
            "--continuation-token",
            token,
            "--json",
        ],
    )?)?;

    assert!(next.get("nextContinuationToken").is_none());

    let mut ids = vec![
        page["principals"][0]["principalId"].clone(),
        next["principals"][0]["principalId"].clone(),
    ];

    let mut expected = vec![first["principalId"].clone(), second["principalId"].clone()];
    ids.sort_by_key(Value::to_string);
    expected.sort_by_key(Value::to_string);
    assert_eq!(ids, expected);
    assert!(page["principals"][0].get("arn").is_some());

    let human = run(root, &["principal", "list", "store", "--max-results=1"])?;
    assert!(String::from_utf8(human.stdout)?.contains("More results: pass --continuation-token"));
    let principal_id = field(&first, "principalId")?;
    let mut secrets = Vec::new();

    for _ in 0..2 {
        let key = success(&run(
            root,
            &[
                "access-key",
                "create",
                "store",
                "--principal-id",
                principal_id,
                "--json",
            ],
        )?)?;

        secrets.push(field(&key, "secretAccessKey")?.to_owned());
    }

    let page = success(&run(
        root,
        &[
            "access-key",
            "list",
            "store",
            "--principal-id",
            principal_id,
            "--max-results=1",
            "--json",
        ],
    )?)?;

    let key_token = field(&page, "nextContinuationToken")?;

    let next = success(&run(
        root,
        &[
            "access-key",
            "list",
            "store",
            "--principal-id",
            principal_id,
            "--continuation-token",
            key_token,
            "--json",
        ],
    )?)?;

    assert!(next.get("nextContinuationToken").is_none());
    assert_ne!(page["accessKeys"][0], next["accessKeys"][0]);

    for secret in secrets {
        assert!(!page.to_string().contains(&secret));
        assert!(!next.to_string().contains(&secret));
    }

    error(
        &run(
            root,
            &[
                "access-key",
                "list",
                "store",
                "--principal-id",
                field(&second, "principalId")?,
                "--continuation-token",
                key_token,
                "--json",
            ],
        )?,
        1,
        "operation",
        "InvalidArgument",
    )?;

    error(
        &run(
            root,
            &["principal", "list", "store", "--max-results=0", "--json"],
        )?,
        2,
        "arguments",
        "InvalidArgument",
    )?;

    error(
        &run(
            root,
            &["principal", "list", "store", "--max-results=1001", "--json"],
        )?,
        2,
        "arguments",
        "InvalidArgument",
    )?;

    directory.finish()?;
    Ok(())
}

#[test]
fn policy_input_is_exact_bounded_and_atomic_for_both_target_kinds() -> TestResult {
    let directory = TestDirectory::new()?;
    let root = directory.path();

    success(&run(
        root,
        &["init", "store", "--bucket=uploads", "--json"],
    )?)?;

    let principal = success(&run(
        root,
        &["principal", "create", "store", "--name=app", "--json"],
    )?)?;

    let principal_id = field(&principal, "principalId")?;

    let bucket_policy = format!(
        "{{\n\"Version\":\"2012-10-17\",\"Statement\":{{\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{}\"}},\"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::uploads/*\"}}\n}}\n",
        field(&principal, "arn")?
    );

    for (target, identity, document) in [
        ("--principal-id", principal_id, IDENTITY_POLICY),
        ("--bucket", "uploads", bucket_policy.as_str()),
    ] {
        let mut invocation = command(
            root,
            &[
                "policy", "apply", "store", target, identity, "--file", "-", "--json",
            ],
        );

        invocation.stdin(Stdio::piped());
        let mut child = Process::spawn(invocation)?;
        child.input(document.as_bytes())?;
        assert_eq!(success(&child.finish()?)?, json!({}));

        assert_eq!(
            run(root, &["policy", "get", "store", target, identity])?.stdout,
            document.as_bytes()
        );

        assert_eq!(
            success(&run(
                root,
                &["policy", "get", "store", target, identity, "--json"]
            )?)?,
            json!({"document": document})
        );

        let bounded = format!("{document}{}", " ".repeat(20 * 1024 - document.len()));
        fs::write(root.join("policy-input"), &bounded)?;

        success(&run(
            root,
            &[
                "policy",
                "apply",
                "store",
                target,
                identity,
                "--file=policy-input",
                "--json",
            ],
        )?)?;

        assert_eq!(
            run(root, &["policy", "get", "store", target, identity])?.stdout,
            bounded.as_bytes()
        );

        fs::write(root.join("policy-input"), document)?;

        success(&run(
            root,
            &[
                "policy",
                "apply",
                "store",
                target,
                identity,
                "--file=policy-input",
                "--json",
            ],
        )?)?;

        let semantic_phase = if target == "--bucket" {
            "operation"
        } else {
            "input"
        };

        let duplicate_fields = br#"{"Version":"2012-10-17","Version":"2012-10-17","Statement":[]}"#;
        let unsupported_action = br#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","NotAction":"s3:GetObject","Resource":"*"}}"#;

        for (case, rejected, phase) in [
            ("invalid JSON", b"not json".to_vec(), "input"),
            ("oversized document", vec![b' '; 20 * 1024 + 1], "input"),
            ("invalid UTF-8", vec![0xff], "input"),
            ("duplicate fields", duplicate_fields.to_vec(), "input"),
            (
                "unsupported action",
                unsupported_action.to_vec(),
                semantic_phase,
            ),
        ] {
            fs::write(root.join("policy-input"), rejected)?;

            let output = run(
                root,
                &[
                    "policy",
                    "apply",
                    "store",
                    target,
                    identity,
                    "--file",
                    "policy-input",
                    "--json",
                ],
            )?;

            assert_eq!(output.status.code(), Some(2), "{target}: {case}");
            error(&output, 2, phase, "InvalidArgument")?;

            assert_eq!(
                run(root, &["policy", "get", "store", target, identity])?.stdout,
                document.as_bytes(),
                "{target}: {case} replaced the stored policy"
            );
        }

        for _ in 0..2 {
            success(&run(
                root,
                &["policy", "delete", "store", target, identity, "--json"],
            )?)?;
        }

        let code = if target == "--bucket" {
            "NoSuchBucketPolicy"
        } else {
            "NoSuchPrincipalPolicy"
        };

        error(
            &run(
                root,
                &["policy", "get", "store", target, identity, "--json"],
            )?,
            1,
            "operation",
            code,
        )?;
    }

    let public = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::uploads/*"}}"#;
    fs::write(root.join("policy-input"), public)?;

    error(
        &run(
            root,
            &[
                "policy",
                "apply",
                "store",
                "--bucket=uploads",
                "--file=policy-input",
                "--json",
            ],
        )?,
        1,
        "operation",
        "AccessDenied",
    )?;

    fs::write(root.join("policy-input"), &bucket_policy)?;

    error(
        &run(
            root,
            &[
                "policy",
                "apply",
                "store",
                "--bucket=missing",
                "--file=policy-input",
                "--json",
            ],
        )?,
        1,
        "operation",
        "NoSuchBucket",
    )?;

    fs::write(
        root.join("policy-input"),
        bucket_policy.replace("uploads", "other-bucket"),
    )?;

    error(
        &run(
            root,
            &[
                "policy",
                "apply",
                "store",
                "--bucket=uploads",
                "--file=policy-input",
                "--json",
            ],
        )?,
        2,
        "operation",
        "InvalidArgument",
    )?;

    error(
        &run(
            root,
            &[
                "policy",
                "apply",
                "store",
                "--bucket=uploads",
                "--file=nonexistent",
                "--json",
            ],
        )?,
        1,
        "input",
        "IoError",
    )?;

    directory.finish()?;
    Ok(())
}
