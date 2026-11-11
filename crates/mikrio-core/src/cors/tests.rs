use super::*;

fn rule() -> CorsRule {
    CorsRule {
        id: None,
        allowed_origins: vec!["https://*.example.com".into()],
        allowed_methods: vec!["GET".into(), "PUT".into()],
        allowed_headers: Some(vec!["x-amz-*".into(), "Content-Type".into()]),
        expose_headers: Some(vec!["ETag".into()]),
        max_age_seconds: Some(0),
    }
}

#[test]
fn rule_fields_and_empty_optional_lists_survive_private_encoding() {
    let mut empty = rule();
    empty.id = Some("上传 & <rule>".into());
    empty.allowed_headers = Some(vec![]);
    empty.expose_headers = None;
    empty.max_age_seconds = Some(i32::MAX);
    let mut negative = rule();
    negative.max_age_seconds = Some(i32::MIN);
    let configuration = CorsConfiguration::new(vec![rule(), empty, negative]).unwrap();
    let encoded = configuration.encode().unwrap();
    assert_eq!(CorsConfiguration::decode(&encoded).unwrap(), configuration);
    assert_eq!(configuration.rules()[0].max_age_seconds, Some(0));
    assert_eq!(configuration.rules()[1].allowed_headers, Some(vec![]));
    assert_eq!(configuration.rules()[1].expose_headers, None);
}

#[test]
fn invalid_rules_and_later_rules_reject_the_complete_configuration() {
    let mut cases = Vec::new();
    let mut candidate = rule();
    candidate.allowed_origins.clear();
    cases.push(candidate);
    let mut candidate = rule();
    candidate.allowed_methods.clear();
    cases.push(candidate);

    for method in ["OPTIONS", "get", "GET,PUT", ""] {
        let mut candidate = rule();
        candidate.allowed_methods = vec![method.into()];
        cases.push(candidate);
    }

    for origin in [
        "",
        "https://*.*.example.com",
        "https://x\r\nInjected: y",
        "a b",
    ] {
        let mut candidate = rule();
        candidate.allowed_origins = vec![origin.into()];
        cases.push(candidate);
    }

    for header in ["", "x-*-*", "x bad", "x:y", "x\n"] {
        let mut candidate = rule();
        candidate.allowed_headers = Some(vec![header.into()]);
        cases.push(candidate);
    }

    let mut candidate = rule();
    candidate.expose_headers = Some(vec!["*".into()]);
    cases.push(candidate);
    let mut candidate = rule();
    candidate.id = Some("a".repeat(256));
    cases.push(candidate);

    for (index, candidate) in cases.into_iter().enumerate() {
        assert_eq!(
            CorsConfiguration::new(vec![rule(), candidate]),
            Err(CorsError),
            "case {index}"
        );
    }

    assert_eq!(CorsConfiguration::new(vec![]), Err(CorsError));
    assert!(CorsConfiguration::new(vec![rule(); 100]).is_ok());
    assert_eq!(CorsConfiguration::new(vec![rule(); 101]), Err(CorsError));
    let mut identified = rule();
    identified.id = Some("a".repeat(255));
    assert!(CorsConfiguration::new(vec![identified.clone()]).is_ok());
    assert_eq!(CorsConfiguration::new(vec![identified; 2]), Err(CorsError));
}

#[test]
fn exact_xml_budget_and_json_expansion_are_bounded() {
    let shell = "<CORSConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><CORSRule><AllowedOrigin></AllowedOrigin><AllowedMethod>GET</AllowedMethod></CORSRule></CORSConfiguration>";
    let available = 65_536 - shell.len();

    let make = |size| CorsRule {
        id: None,
        allowed_origins: vec!["\\".repeat(size)],
        allowed_methods: vec!["GET".into()],
        allowed_headers: None,
        expose_headers: None,
        max_age_seconds: None,
    };

    for size in [available - 1, available] {
        let configuration = CorsConfiguration::new(vec![make(size)]).unwrap();
        let encoded = configuration.encode().unwrap();
        assert!(encoded.len() > 65_536, "size={size}");
        assert!(encoded.len() <= 262_144, "size={size}");

        assert_eq!(
            CorsConfiguration::decode(&encoded).unwrap(),
            configuration,
            "size={size}"
        );
    }

    assert_eq!(
        CorsConfiguration::new(vec![make(available + 1)]),
        Err(CorsError)
    );

    let mut escaped = make(available / 5 + 1);
    escaped.allowed_origins[0] = "&".repeat(available / 5 + 1);
    assert_eq!(CorsConfiguration::new(vec![escaped]), Err(CorsError));
}

#[test]
fn xml_budget_counts_escaped_quotes_at_the_document_boundary() {
    let shell = "<CORSConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><CORSRule><AllowedOrigin></AllowedOrigin><AllowedMethod>GET</AllowedMethod></CORSRule></CORSConfiguration>";
    let available = 65_536 - shell.len();

    for quote in ['\'', '"'] {
        for extra in [-1_isize, 0, 1] {
            let escaped_bytes = available.checked_add_signed(extra).unwrap();

            let origin = format!(
                "{}{}",
                quote.to_string().repeat(escaped_bytes / 6),
                "x".repeat(escaped_bytes % 6)
            );

            let candidate = CorsRule {
                id: None,
                allowed_origins: vec![origin],
                allowed_methods: vec!["GET".into()],
                allowed_headers: None,
                expose_headers: None,
                max_age_seconds: None,
            };

            assert_eq!(
                CorsConfiguration::new(vec![candidate]).is_ok(),
                extra <= 0,
                "quote {quote:?}, escaped document delta {extra}"
            );
        }
    }
}

#[test]
fn corrupt_stored_fields_are_not_defaulted_or_ignored() {
    for encoded in [
        "null",
        "[]",
        "{}",
        "[{}]",
        r#"[{"allowed_origins":["*"],"allowed_methods":["GET"],"unknown":1}]"#,
        r#"[{"allowed_origins":["*"],"allowed_methods":["GET"],"max_age_seconds":2147483648}]"#,
        r#"[{"allowed_origins":["*"],"allowed_methods":["GET"],"max_age_seconds":true}]"#,
        r#"[{"allowed_origins":["*"],"allowed_methods":["GET"],"id":"a","id":"b"}]"#,
    ] {
        assert_eq!(
            CorsConfiguration::decode(encoded),
            Err(CorsError),
            "{encoded}"
        );
    }
}
