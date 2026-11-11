use super::*;
use std::fmt::Write as _;

#[derive(Deserialize)]
struct Fixtures {
    valid: Vec<(String, String)>,
    invalid_keys: Vec<String>,
    invalid_values: Vec<String>,
    stored: String,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!(
        "../../../../../tests/contract/object-tags.json"
    ))
    .unwrap()
}

#[test]
fn tags_preserve_exact_case_and_unicode_and_reject_unsupported_categories() {
    let fixtures = fixtures();

    for (key, value) in fixtures.valid {
        let tags = ObjectTags::new([(key.clone(), value.clone())]).unwrap();
        assert_eq!(tags.entries()[&key], value);
    }

    for key in fixtures.invalid_keys {
        assert_eq!(
            ObjectTags::new([(key.clone(), String::new())]),
            Err(ObjectValueError::TagKey),
            "key {key:?}"
        );
    }

    for value in fixtures.invalid_values {
        assert_eq!(
            ObjectTags::new([("name".into(), value.clone())]),
            Err(ObjectValueError::TagValue),
            "value {value:?}"
        );
    }
}

#[test]
fn tags_count_utf16_units_and_reject_duplicates_before_map_conversion() {
    for (character, key_limit, value_limit) in [("x", 128, 256), ("é", 128, 256), ("𐐀", 64, 128)]
    {
        let key = character.repeat(key_limit);
        let value = character.repeat(value_limit);
        let tags = ObjectTags::new([(key.clone(), value.clone())]).unwrap();
        assert_eq!(tags.entries()[&key], value);

        assert_eq!(
            ObjectTags::new([(format!("{key}x"), value.clone())]),
            Err(ObjectValueError::TagKey),
            "character={character:?}, key_limit={key_limit}"
        );

        assert_eq!(
            ObjectTags::new([(key, format!("{value}x"))]),
            Err(ObjectValueError::TagValue),
            "character={character:?}, value_limit={value_limit}"
        );
    }

    let tags =
        ObjectTags::new((0..10).map(|index| (format!("tag{index}"), String::new()))).unwrap();

    assert_eq!(tags.entries().len(), 10);

    assert_eq!(
        ObjectTags::new((0..11).map(|index| (format!("tag{index}"), String::new()))),
        Err(ObjectValueError::TagCount)
    );

    assert_eq!(
        ObjectTags::new([
            ("name".into(), "first".into()),
            ("name".into(), "second".into())
        ]),
        Err(ObjectValueError::TagDuplicate)
    );
}

#[test]
fn tag_json_is_deterministic_and_never_discards_duplicates_or_corruption() {
    let tags = ObjectTags::new([
        ("𐐀".into(), "Ⅳ".into()),
        ("é".into(), "日本語".into()),
        ("name".into(), "É".into()),
        ("Name".into(), String::new()),
    ])
    .unwrap();

    let encoded = tags.to_json().unwrap();
    assert_eq!(encoded, fixtures().stored);
    assert_eq!(ObjectTags::from_json(&encoded).unwrap(), tags);
    assert_eq!(ObjectTags::default().to_json().unwrap(), "{}");

    for encoded in [
        r#"{"name":"first","name":"second"}"#,
        r#"{"name":"first","\u006eame":"second"}"#,
        r#"{"name":null}"#,
        r#"{"name":1}"#,
        r#"{"name":[]}"#,
        r#"{"":"value"}"#,
        r#"{"aws:key":"value"}"#,
        r#"{"name":"\t"}"#,
        r#"{"name":"\ud800"}"#,
        "[]",
        "null",
        "{} {}",
    ] {
        assert_eq!(
            ObjectTags::from_json(encoded),
            Err(ObjectValueError::TagEncoding),
            "accepted {encoded:?}"
        );
    }

    assert_eq!(
        ObjectTags::from_json(&format!("{}{{}}", " ".repeat(32_767))),
        Err(ObjectValueError::TagEncoding)
    );
}

#[test]
fn maximum_escaped_tag_set_fits_the_persisted_envelope() {
    let entries = (0..10).map(|index| (format!("{index}{}", "日".repeat(127)), "本".repeat(256)));
    let tags = ObjectTags::new(entries).unwrap();
    let encoded = tags.to_json().unwrap();
    assert!(encoded.len() < 32_768);

    // A different JSON serializer may escape every non-ASCII UTF-16 unit.
    let mut escaped = String::new();

    for character in encoded.chars() {
        if character.is_ascii() {
            escaped.push(character);
            continue;
        }

        for unit in character.encode_utf16(&mut [0; 2]) {
            write!(escaped, "\\u{unit:04x}").unwrap();
        }
    }

    assert_eq!(escaped.len(), 23_051);
    assert_eq!(ObjectTags::from_json(&escaped).unwrap(), tags);
}

#[test]
fn reserved_prefix_rules_match_for_new_and_persisted_tags() {
    for key in [
        "aws:reserved",
        "awS:reserved",
        "aWs:reserved",
        "aWS:reserved",
        "Aws:reserved",
        "AwS:reserved",
        "AWs:reserved",
        "AWS:reserved",
    ] {
        assert_eq!(
            ObjectTags::new([(key.into(), String::new())]),
            Err(ObjectValueError::TagKey),
            "key={key}"
        );

        let encoded = format!(r#"{{"{key}":""}}"#);
        assert_eq!(
            ObjectTags::from_json(&encoded),
            Err(ObjectValueError::TagEncoding),
            "key={key}"
        );
    }
}
