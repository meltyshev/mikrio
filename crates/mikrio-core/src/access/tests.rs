use super::*;

mod limits;

const ACCOUNT: &str = "012345678901";
const PRINCIPAL: &str = "arn:aws:iam::012345678901:user/000102030405060708090a0b0c0d0e0f";
const OBJECT: &str = "arn:aws:s3:::photos/album/photo.jpg";

fn identity(statement: &str) -> Policy {
    Policy::parse(
        &format!(r#"{{"Version":"2012-10-17","Statement":{statement}}}"#),
        PolicyKind::Identity,
    )
    .unwrap()
}

fn bucket(statement: &str) -> Result<Policy, PolicyError> {
    Policy::parse(
        &format!(r#"{{"Version":"2012-10-17","Statement":{statement}}}"#),
        PolicyKind::Bucket {
            name: "photos",
            owner_account_id: ACCOUNT,
        },
    )
}

fn condition(operator: &str, key: &str, expected: &str) -> Policy {
    identity(&format!(
        r#"{{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{{"{operator}":{{"{key}":{expected}}}}}}}"#
    ))
}

fn evaluate(policy: &Policy, context: &Context) -> Verdict {
    policy
        .evaluate(
            Action::parse("s3:GetObject").unwrap(),
            OBJECT,
            Some(PRINCIPAL),
            context,
        )
        .unwrap()
}

#[test]
fn whole_document_rejects_duplicates_late_invalid_statements_and_unsupported_forms() {
    let cases = [
        (
            r#"{"Version":"2012-10-17","Version":"2012-10-17","Statement":{}}"#,
            PolicyError::Invalid,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Effect":"Deny"}}"#,
            PolicyError::Invalid,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{"StringEquals":{"s3:prefix":"a","s3:prefix":"b"}}}}"#,
            PolicyError::Invalid,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"},{"Effect":"Allow","Action":"iam:*","Resource":"*"}]}"#,
            PolicyError::Unsupported,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","NotAction":"s3:DeleteObject","Resource":"*"}}"#,
            PolicyError::Unsupported,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Principal":"*"}}"#,
            PolicyError::Unsupported,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":[],"Resource":"*"}}"#,
            PolicyError::Invalid,
        ),
        (
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::${bucket}/*"}}"#,
            PolicyError::Unsupported,
        ),
    ];

    for (document, error) in cases {
        assert_eq!(
            Policy::parse(document, PolicyKind::Identity).err(),
            Some(error),
            "document={document}"
        );
    }

    let deeply_nested = format!("{}null{}", "[".repeat(13), "]".repeat(13));

    assert!(matches!(
        Policy::parse(&deeply_nested, PolicyKind::Identity),
        Err(PolicyError::Invalid)
    ));

    assert!(matches!(
        Policy::parse(&" ".repeat(20481), PolicyKind::Identity),
        Err(PolicyError::TooLarge)
    ));
}

#[test]
fn exact_input_is_retained_and_actions_are_case_insensitive_but_resources_are_not() {
    let document = " {\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Allow\",\"Action\":\"S3:gEt?bject\",\"Resource\":\"arn:aws:s3:::photos/album/*\"}}\n";
    let policy = Policy::parse(document, PolicyKind::Identity).unwrap();
    assert_eq!(policy.document(), document);
    assert_eq!(evaluate(&policy, &Context::default()), Verdict::Allow);

    assert_eq!(
        policy
            .evaluate(
                Action::parse("s3:GetObject").unwrap(),
                "arn:aws:s3:::photos/Album/photo.jpg",
                Some(PRINCIPAL),
                &Context::default()
            )
            .unwrap(),
        Verdict::ImplicitDeny
    );

    assert_eq!(
        policy
            .evaluate(
                Action::parse("s3:GetObject").unwrap(),
                OBJECT,
                None,
                &Context::default()
            )
            .unwrap(),
        Verdict::ImplicitDeny
    );
}

#[test]
fn explicit_deny_overrides_allow_in_either_statement_order() {
    let allow = r#"{"Effect":"Allow","Action":"s3:*","Resource":"*"}"#;
    let deny = r#"{"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}"#;

    for statements in [format!("[{allow},{deny}]"), format!("[{deny},{allow}]")] {
        assert_eq!(
            evaluate(&identity(&statements), &Context::default()),
            Verdict::ExplicitDeny,
            "statements={statements}"
        );
    }
}

#[test]
fn public_read_is_classified_separately_and_never_escapes_its_bucket_attachment() {
    let public =
        bucket(r#"{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"*"}"#)
            .unwrap();

    assert!(public.is_public());
    let action = Action::parse("s3:GetObject").unwrap();

    assert_eq!(
        public
            .evaluate(action, OBJECT, None, &Context::default())
            .unwrap(),
        Verdict::Allow
    );

    assert_eq!(
        public
            .evaluate(
                action,
                "arn:aws:s3:::photos-other/key",
                None,
                &Context::default()
            )
            .unwrap(),
        Verdict::ImplicitDeny
    );

    assert!(matches!(
        bucket(r#"{"Effect":"Allow","Principal":"*","Action":"s3:PutObject","Resource":"*"}"#),
        Err(PolicyError::Unsupported)
    ));

    let denial =
        bucket(r#"{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"*"}"#).unwrap();

    assert!(!denial.is_public());

    assert_eq!(
        denial
            .evaluate(action, OBJECT, None, &Context::default())
            .unwrap(),
        Verdict::ExplicitDeny
    );
}

#[test]
fn exact_local_principals_do_not_rebind_by_label_or_accept_account_root() {
    let policy = bucket(&format!(
        r#"{{"Effect":"Allow","Principal":{{"AWS":"{PRINCIPAL}"}},"Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#
    ))
    .unwrap();

    assert!(!policy.is_public());
    assert_eq!(evaluate(&policy, &Context::default()), Verdict::Allow);
    let other = "arn:aws:iam::012345678901:user/ffffffffffffffffffffffffffffffff";

    assert_eq!(
        policy
            .evaluate(
                Action::parse("s3:GetObject").unwrap(),
                OBJECT,
                Some(other),
                &Context::default()
            )
            .unwrap(),
        Verdict::ImplicitDeny
    );

    for invalid in [
        "alice",
        "arn:aws:iam::012345678901:root",
        "arn:aws:iam::012345678901:user/*",
        "arn:aws:iam::999999999999:user/000102030405060708090a0b0c0d0e0f",
    ] {
        assert!(
            matches!(
                bucket(&format!(
                    r#"{{"Effect":"Allow","Principal":{{"AWS":"{invalid}"}},"Action":"s3:GetObject","Resource":"*"}}"#
                )),
                Err(PolicyError::WrongScope)
            ),
            "principal={invalid:?}"
        );
    }
}

#[test]
fn missing_empty_and_multiple_policy_values_follow_independent_condition_truth_table() {
    // Literal expectations from IAM Condition operators, not evaluator output.
    // None is absent; Some("") is a present empty string.
    let cases = [
        ("StringEquals", None, false),
        ("StringEquals", Some(""), false),
        ("StringEquals", Some("a"), true),
        ("StringEquals", Some("b"), true),
        ("StringNotEquals", None, true),
        ("StringNotEquals", Some("a"), false),
        ("StringNotEquals", Some("c"), true),
        ("StringEqualsIfExists", None, true),
        ("StringNotEqualsIfExists", None, true),
        ("StringLike", Some("a"), true),
        ("StringNotLike", Some("a"), false),
        ("StringNotLike", None, true),
    ];

    for (operator, value, allowed) in cases {
        let policy = condition(operator, "s3:prefix", r#"["a","b"]"#);
        let mut context = Context::default();

        if let Some(value) = value {
            context.insert("s3:prefix", vec![value.into()]).unwrap();
        }

        let expected = if allowed {
            Verdict::Allow
        } else {
            Verdict::ImplicitDeny
        };

        assert_eq!(
            evaluate(&policy, &context),
            expected,
            "{operator} {value:?}"
        );
    }

    let empty = condition("StringEquals", "s3:prefix", r#""""#);
    let mut context = Context::default();
    context.insert("s3:prefix", vec![String::new()]).unwrap();
    assert_eq!(evaluate(&empty, &context), Verdict::Allow);

    assert_eq!(
        evaluate(&condition("Null", "s3:prefix", "false"), &context),
        Verdict::Allow
    );

    assert_eq!(
        evaluate(&condition("Null", "s3:prefix", "true"), &Context::default()),
        Verdict::Allow
    );
}

#[test]
fn tag_whitelist_requires_all_members_and_explicit_existence() {
    let all = condition(
        "ForAllValues:StringEquals",
        "s3:RequestObjectTagKeys",
        r#"["project","owner"]"#,
    );

    let any = condition(
        "ForAnyValue:StringEquals",
        "s3:RequestObjectTagKeys",
        r#"["project","owner"]"#,
    );

    let mut context = Context::default();

    context
        .insert(
            "s3:RequestObjectTagKeys",
            vec!["project".into(), "visibility".into()],
        )
        .unwrap();

    assert_eq!(evaluate(&all, &context), Verdict::ImplicitDeny);
    assert_eq!(evaluate(&any, &context), Verdict::Allow);
    assert_eq!(evaluate(&all, &Context::default()), Verdict::Allow);
    assert_eq!(evaluate(&any, &Context::default()), Verdict::ImplicitDeny);

    let guarded = identity(
        r#"{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{"ForAllValues:StringEquals":{"s3:RequestObjectTagKeys":["project","owner"]},"Null":{"s3:RequestObjectTagKeys":"false"}}}"#,
    );

    assert_eq!(
        evaluate(&guarded, &Context::default()),
        Verdict::ImplicitDeny
    );

    let tag = condition("StringEquals", "s3:ExistingObjectTag/project", r#""alpha""#);
    let mut context = Context::default();

    context
        .insert("s3:ExistingObjectTag/project", vec!["alpha".into()])
        .unwrap();

    assert_eq!(evaluate(&tag, &context), Verdict::Allow);

    assert!(matches!(
        context.insert("s3:ExistingObjectTag/Project", vec!["alpha".into()]),
        Err(PolicyError::Invalid)
    ));
}

#[test]
fn wildcard_matcher_preserves_slashes_unicode_and_has_no_backtracking_explosion() {
    let cases = [
        ("a/*", "a/b/c", true),
        ("a/?", "a/😀", true),
        ("a/?", "a/e\u{301}", false),
        ("a//b", "a/b", false),
        ("a/../b", "b", false),
        ("*a?b", "aaab", true),
        ("", "", true),
        ("*", "", true),
        ("*", "a/../😀\u{301}", true),
        ("***", "", true),
        ("a**?***b", "a/b", true),
        ("a**?***b", "ab", false),
        ("?", "", false),
    ];

    for (pattern, value, expected) in cases {
        assert_eq!(
            pattern::matches(pattern, value),
            expected,
            "pattern={pattern:?}, value={value:?}"
        );
    }

    assert!(pattern::matches(&"*".repeat(20_000), &"a".repeat(1024)));

    assert!(!pattern::matches(
        &format!("{}z", "*a".repeat(1000)),
        &"a".repeat(1024)
    ));
}

#[test]
fn operator_types_set_qualifiers_and_duplicate_context_are_rejected() {
    for (operator, key, value) in [
        ("StringEquals", "s3:prefix", "true"),
        ("Bool", "s3:prefix", "true"),
        ("StringLike", "s3:ObjectCreationOperation", r#""*""#),
        ("NullIfExists", "s3:prefix", "true"),
        ("ForAnyValue:StringLike", "s3:prefix", r#""*""#),
        (
            "ForAllValues:StringEqualsIfExists",
            "s3:RequestObjectTagKeys",
            r#""project""#,
        ),
    ] {
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{{"{operator}":{{"{key}":{value}}}}}}}}}"#
        );

        let expected = if operator == "StringEquals" {
            PolicyError::Invalid
        } else {
            PolicyError::Unsupported
        };

        assert!(
            matches!(Policy::parse(&document, PolicyKind::Identity), Err(error) if error == expected),
            "operator={operator}, key={key}, value={value}"
        );
    }

    let mut context = Context::default();
    context.insert("S3:PREFIX", vec!["one".into()]).unwrap();

    assert_eq!(
        context.insert("s3:prefix", vec!["two".into()]),
        Err(PolicyError::Invalid)
    );

    assert_eq!(
        context.insert("s3:ObjectCreationOperation", vec!["invalid".into()]),
        Err(PolicyError::Invalid)
    );
}

#[test]
fn credential_classes_and_secret_values_have_bounded_redacted_representations() {
    assert_eq!(
        CredentialClass::parse("mikrio_a00112233445566778899AABBCCDDEEFF"),
        Ok(CredentialClass::Application)
    );

    for (case_index, invalid) in [
        "mikrio_aunknown",
        "mikrio_a00112233445566778899AABBCCDDEEF",
        "mikrio_a00112233445566778899AABBCCDDEEFF0",
        "mikrio_a00112233445566778899aabbccddeeff",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            CredentialClass::parse(invalid),
            Err(IdentityError::CredentialEncoding),
            "case {case_index}: malformed application keys cannot fall back to bootstrap"
        );
    }

    assert_eq!(
        CredentialClass::parse("mikrio_b00112233445566778899AABBCCDDEEFF"),
        Ok(CredentialClass::Bootstrap)
    );

    assert_eq!(
        CredentialClass::parse(&"mikrio_a-custom".to_ascii_uppercase()),
        Ok(CredentialClass::Bootstrap)
    );

    for (case_index, invalid) in ["", "has space", "path/segment", "é", "line\nfeed"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            CredentialClass::parse(invalid),
            Err(IdentityError::CredentialEncoding),
            "case {case_index}: invalid credential encoding"
        );
    }

    let secret = Secret::new("secret-key signed-url body-canary".into()).unwrap();
    assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");

    assert!(matches!(
        Secret::new(String::new()),
        Err(IdentityError::SecretLength)
    ));

    assert!(matches!(
        Secret::new("x".repeat(1025)),
        Err(IdentityError::SecretLength)
    ));
}

#[test]
fn exact_byte_ceiling_accepts_a_complete_document_and_bounds_adversarial_matching() {
    let pattern = format!("arn:aws:s3:::photos/{}z", "*a".repeat(9800));

    let mut document = format!(
        r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Action":"s3:GetObject","Resource":"{pattern}"}}}}"#
    );

    document.push_str(&" ".repeat(20480 - document.len()));
    let policy = Policy::parse(&document, PolicyKind::Identity).unwrap();
    assert_eq!(policy.document().len(), 20480);
    let resource = format!("arn:aws:s3:::photos/{}", "a".repeat(1024));

    assert_eq!(
        policy
            .evaluate(
                Action::parse("s3:GetObject").unwrap(),
                &resource,
                Some(PRINCIPAL),
                &Context::default()
            )
            .unwrap(),
        Verdict::ImplicitDeny
    );

    document.push(' ');

    assert!(matches!(
        Policy::parse(&document, PolicyKind::Identity),
        Err(PolicyError::TooLarge)
    ));
}

#[test]
fn conditional_creation_and_ifexists_denials_preserve_missing_context() {
    let policy = identity(
        r#"[
        {"Effect":"Allow","Action":"s3:PutObject","Resource":"*"},
        {"Effect":"Deny","Action":"s3:PutObject","Resource":"*","Condition":{
            "Null":{"s3:if-none-match":"true"},
            "Bool":{"s3:ObjectCreationOperation":"true"}
        }}
    ]"#,
    );

    let action = Action::parse("s3:PutObject").unwrap();

    // Literal expected table: the boolean comes from the operation, not its verb.
    for (creation, condition, expected) in [
        (Some("true"), None, Verdict::ExplicitDeny),
        (Some("true"), Some("*"), Verdict::Allow),
        (Some("false"), None, Verdict::Allow),
        (None, None, Verdict::Allow),
    ] {
        let mut context = Context::default();

        if let Some(value) = creation {
            context
                .insert("s3:ObjectCreationOperation", vec![value.into()])
                .unwrap();
        }

        if let Some(value) = condition {
            context
                .insert("s3:if-none-match", vec![value.into()])
                .unwrap();
        }

        assert_eq!(
            policy
                .evaluate(action, OBJECT, Some(PRINCIPAL), &context)
                .unwrap(),
            expected,
            "creation={creation:?}, condition={condition:?}"
        );
    }

    let deny = identity(
        r#"{"Effect":"Deny","Action":"s3:GetObject","Resource":"*",
        "Condition":{"StringNotEqualsIfExists":{"s3:ExistingObjectTag/visibility":"public"}}}"#,
    );

    for (value, expected) in [
        (None, Verdict::ExplicitDeny),
        (Some(""), Verdict::ExplicitDeny),
        (Some("private"), Verdict::ExplicitDeny),
        (Some("public"), Verdict::ImplicitDeny),
    ] {
        let mut context = Context::default();

        if let Some(value) = value {
            context
                .insert("s3:ExistingObjectTag/visibility", vec![value.into()])
                .unwrap();
        }

        assert_eq!(evaluate(&deny, &context), expected, "tag={value:?}");
    }
}

#[test]
fn repeated_action_patterns_expand_only_to_the_fixed_permission_set() {
    let patterns = vec!["s3:*"; 2_000];

    let document = serde_json::json!({"Version":"2012-10-17","Statement":{
        "Effect":"Allow","Action":patterns,"Resource":"*"
    }})
    .to_string();

    let policy = Policy::parse(&document, PolicyKind::Identity).unwrap();
    assert_eq!(policy.statements[0].actions.len(), ACTIONS.len());
    assert_eq!(evaluate(&policy, &Context::default()), Verdict::Allow);
}

#[test]
fn condition_case_aliases_and_unqualified_tag_suffixes_fail_explicitly() {
    let document = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{"StringEquals":{"s3:prefix":"a","S3:PREFIX":"b"}}}}"#;

    assert_eq!(
        Policy::parse(document, PolicyKind::Identity).err(),
        Some(PolicyError::Invalid)
    );

    let mut context = Context::default();

    context
        .insert("s3:ExistingObjectTag/Project", vec!["value".into()])
        .unwrap();

    let mixed = condition("StringEquals", "S3:EXISTINGOBJECTTAG/pRoJeCt", r#""value""#);
    assert_eq!(evaluate(&mixed, &context), Verdict::Allow);

    assert_eq!(
        context.insert("s3:ExistingObjectTag/project", vec!["other".into()]),
        Err(PolicyError::Invalid)
    );

    assert_eq!(
        context.insert("s3:ExistingObjectTag/é", vec!["value".into()]),
        Err(PolicyError::Unsupported)
    );
}

#[test]
fn documented_condition_fixtures_match_without_rewriting_expected_values() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/contract/access-policy.json"
    ))
    .unwrap();

    assert_eq!(fixture["origin"], "documented-expectation");
    let cases = fixture["cases"].as_array().unwrap();
    assert_ne!(cases.len(), 0);

    for case in cases {
        let policy = condition(
            case["operator"].as_str().unwrap(),
            case["key"].as_str().unwrap(),
            &case["policy_values"].to_string(),
        );

        let mut context = Context::default();

        if !case["actual"].is_null() {
            let values = case["actual"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect();

            context
                .insert(case["key"].as_str().unwrap(), values)
                .unwrap();
        }

        let expected = [
            ("Allow", Verdict::Allow),
            ("ImplicitDeny", Verdict::ImplicitDeny),
        ]
        .into_iter()
        .find(|(name, _)| Some(*name) == case["expected"].as_str())
        .expect("unknown fixture verdict")
        .1;

        assert_eq!(evaluate(&policy, &context), expected, "{}", case["name"]);
    }
}
