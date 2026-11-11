use super::*;

fn rule(origin: &str, header: &str) -> CorsRule {
    CorsRule {
        id: None,
        allowed_origins: vec![origin.into()],
        allowed_methods: vec!["PUT".into()],
        allowed_headers: Some(vec![header.into()]),
        expose_headers: Some(vec!["ETag".into()]),
        max_age_seconds: None,
    }
}

#[test]
fn preflight_selects_first_complete_rule_without_combining_allowed_headers() {
    let rules = [rule("https://*.example.com", "x-one"), rule("*", "x-two")];

    assert_eq!(
        matching(&rules, "https://app.example.com", "PUT", &["X-ONE".into()]),
        Some(&rules[0])
    );

    assert_eq!(
        matching(&rules, "https://app.example.com", "PUT", &["x-two".into()]),
        Some(&rules[1])
    );

    assert_eq!(
        matching(
            &rules,
            "https://app.example.com",
            "PUT",
            &["x-one".into(), "x-two".into()]
        ),
        None
    );

    assert_eq!(
        matching(&rules, "https://app.example.com", "put", &[]),
        None
    );

    assert_eq!(
        matching(&rules[..1], "https://APP.EXAMPLE.COM", "PUT", &[]),
        None
    );

    assert_eq!(
        matching(&rules, "https://app.example.com", "PUT", &[]),
        Some(&rules[0])
    );
}

#[test]
fn response_projection_uses_only_selected_rule_and_retains_zero_max_age_inputs() {
    let mut selected = rule("https://*.example.com", "*");
    selected.max_age_seconds = Some(0);
    let mut headers = HeaderMap::new();
    project(&mut headers, &selected, "https://app.example.com").unwrap();

    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example.com"
    );

    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
    assert_eq!(headers[header::ACCESS_CONTROL_EXPOSE_HEADERS], "ETag");

    // The first matching origin in the selected rule controls reflection.
    selected.allowed_origins.push("*".into());
    let mut headers = HeaderMap::new();
    project(&mut headers, &selected, "https://app.example.com").unwrap();

    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example.com"
    );

    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");

    let mut headers = HeaderMap::new();
    project(&mut headers, &rule("*", "*"), "null").unwrap();
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
}

#[test]
fn preflight_uses_one_percent_decode_and_rejects_malformed_header_lists() {
    assert_eq!(
        bucket(&Uri::from_static("/ph%6Ftos/a%2Fb"))
            .unwrap()
            .as_str(),
        "photos"
    );

    for path in ["/ph%256Ftos/a", "/photos/%FF"] {
        assert_eq!(
            bucket(&Uri::from_static(path)).unwrap_err().code(),
            &s3s::S3ErrorCode::InvalidURI,
            "{path}"
        );
    }

    for raw in ["", "x-one,,x-two", "x bad", "*", "x:bad"] {
        let mut headers = HeaderMap::new();

        headers.insert(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            HeaderValue::from_str(raw).unwrap(),
        );

        assert_eq!(
            requested_headers(&headers).unwrap_err().code(),
            &s3s::S3ErrorCode::InvalidRequest,
            "{raw}"
        );
    }
}
