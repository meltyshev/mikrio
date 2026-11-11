use super::*;

fn rule() -> LifecycleRule {
    LifecycleRule {
        id: None,
        is_enabled: true,
        filter: LifecycleFilter::All,
        expiration_days: Some(1),
        abort_incomplete_multipart_days: None,
    }
}

#[test]
fn ordered_filter_representations_and_disabled_actions_remain_distinct() {
    let rules = [
        LifecycleFilter::All,
        LifecycleFilter::Prefix(String::new()),
        LifecycleFilter::LegacyPrefix(String::new()),
        LifecycleFilter::Prefix("../a//é\r".into()),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, filter)| LifecycleRule {
        id: Some(index.to_string()),
        is_enabled: false,
        filter,
        expiration_days: Some(2_147_483_647),
        abort_incomplete_multipart_days: Some(1),
    })
    .collect::<Vec<_>>();

    let configuration = LifecycleConfiguration::new(rules.clone()).unwrap();
    assert_eq!(configuration.rules(), rules);
    assert_eq!(configuration.into_rules(), rules);
}

#[test]
fn whole_configuration_rejects_later_invalid_rules_and_duplicate_explicit_ids() {
    let valid = LifecycleRule {
        id: Some("same".into()),
        ..rule()
    };

    let invalid = [
        valid.clone(),
        LifecycleRule {
            expiration_days: None,
            ..rule()
        },
        LifecycleRule {
            expiration_days: Some(0),
            ..rule()
        },
        LifecycleRule {
            abort_incomplete_multipart_days: Some(0),
            ..rule()
        },
        LifecycleRule {
            expiration_days: Some(2_147_483_648),
            ..rule()
        },
        LifecycleRule {
            abort_incomplete_multipart_days: Some(u32::MAX),
            ..rule()
        },
        LifecycleRule {
            id: Some("a".repeat(256)),
            ..rule()
        },
        LifecycleRule {
            id: Some("\0".into()),
            ..rule()
        },
        LifecycleRule {
            filter: LifecycleFilter::Prefix("a".repeat(1_025)),
            ..rule()
        },
        LifecycleRule {
            filter: LifecycleFilter::LegacyPrefix("\u{ffff}".into()),
            ..rule()
        },
    ];

    for later in invalid {
        assert!(
            LifecycleConfiguration::new(vec![valid.clone(), later.clone()]).is_err(),
            "{later:?}"
        );
    }

    for invalid_text in [
        '\0', '\u{8}', '\u{b}', '\u{c}', '\u{1f}', '\u{fffe}', '\u{ffff}',
    ] {
        assert!(
            LifecycleConfiguration::new(vec![LifecycleRule {
                filter: LifecycleFilter::Prefix(invalid_text.to_string()),
                ..rule()
            }])
            .is_err(),
            "invalid_text={invalid_text:?}"
        );
    }
}

#[test]
fn rule_id_character_and_prefix_byte_limits_are_independent() {
    assert!(LifecycleConfiguration::new(vec![]).is_err());
    assert!(LifecycleConfiguration::new(vec![rule(); 1_000]).is_ok());
    assert!(LifecycleConfiguration::new(vec![rule(); 1_001]).is_err());

    for count in [254, 255, 256] {
        let result = LifecycleConfiguration::new(vec![LifecycleRule {
            id: Some("🦀".repeat(count)),
            ..rule()
        }]);

        assert_eq!(result.is_ok(), count <= 255, "count={count}");
    }

    for count in [1_023, 1_024, 1_025] {
        let prefix = "é".repeat(count / 2) + &"a".repeat(count % 2);
        assert_eq!(prefix.len(), count);

        assert_eq!(
            LifecycleConfiguration::new(vec![LifecycleRule {
                filter: LifecycleFilter::Prefix(prefix),
                ..rule()
            }])
            .is_ok(),
            count <= 1_024,
            "count={count}"
        );
    }

    let generated = vec![
        rule(),
        rule(),
        LifecycleRule {
            id: Some(String::new()),
            ..rule()
        },
    ];

    assert!(LifecycleConfiguration::new(generated).is_ok());

    assert!(
        LifecycleConfiguration::new(vec![LifecycleRule {
            id: Some("\t\n\r&<>\"' 🦀".into()),
            ..rule()
        }])
        .is_ok()
    );
}
