use super::{ReadConditionFailure as Failure, ReadConditions, ReadConditionsInput};
use crate::EpochMillis;

#[test]
fn authorized_read_conditions_follow_strong_weak_list_and_date_precedence() {
    // AWS GetObject condition pairs and RFC 9110 sections 13.1/13.2.2.
    // Expectations are independent literal cases, not evaluator-derived fixtures.
    let cases = [
        (Some("\"current\""), None, None, Some(0), Ok(())),
        (
            Some("W/\"current\""),
            None,
            None,
            None,
            Err(Failure::PreconditionFailed),
        ),
        (Some("\"other\", \"current\""), None, None, None, Ok(())),
        (
            Some("\"current,other\""),
            None,
            None,
            None,
            Err(Failure::PreconditionFailed),
        ),
        (Some("*"), None, None, None, Ok(())),
        (
            None,
            Some("W/\"current\""),
            Some(0),
            None,
            Err(Failure::NotModified),
        ),
        (None, Some("\"different\""), Some(2000), None, Ok(())),
        (None, Some("*"), None, None, Err(Failure::NotModified)),
        (None, None, Some(1000), None, Err(Failure::NotModified)),
        (None, None, Some(0), None, Ok(())),
        (None, None, None, Some(1000), Ok(())),
        (None, None, None, Some(0), Err(Failure::PreconditionFailed)),
        (
            Some("\"wrong\""),
            Some("*"),
            None,
            None,
            Err(Failure::PreconditionFailed),
        ),
    ];

    for (if_match, if_none_match, modified, unmodified, expected) in cases {
        let input = ReadConditionsInput {
            if_match: if_match.map(str::to_owned),
            if_none_match: if_none_match.map(str::to_owned),
            if_modified_since: modified.map(|value| EpochMillis::new(value).unwrap()),
            if_unmodified_since: unmodified.map(|value| EpochMillis::new(value).unwrap()),
        };

        let conditions = ReadConditions::new(input.clone()).unwrap();

        assert_eq!(
            conditions.evaluate("\"current\"", EpochMillis::new(1999).unwrap()),
            expected,
            "{input:?}"
        );
    }

    let conditions = ReadConditions::new(ReadConditionsInput {
        if_match: Some(" W/\"other\", , \"comma,inside\", ".into()),
        ..ReadConditionsInput::default()
    })
    .unwrap();

    assert_eq!(
        conditions.evaluate("\"comma,inside\"", EpochMillis::new(-1).unwrap()),
        Ok(())
    );

    let conditions = ReadConditions::new(ReadConditionsInput {
        if_modified_since: Some(EpochMillis::new(-1000).unwrap()),
        ..ReadConditionsInput::default()
    })
    .unwrap();

    assert_eq!(
        conditions.evaluate("\"current\"", EpochMillis::new(-1).unwrap()),
        Err(Failure::NotModified)
    );
}
