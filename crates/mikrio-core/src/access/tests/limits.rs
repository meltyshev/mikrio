use std::time::Instant;

use serde_json::{Value, json};

use super::*;

fn full_document(statement: &Value) -> String {
    let mut value = json!({
        "Version": "2012-10-17",
        "Id": "",
        "Statement": statement,
    });

    let length = serde_json::to_string(&value).unwrap().len();
    // Only the remainder between the final full pattern and the public byte
    // ceiling is padding. Most bytes participate in the evaluated expressions.
    assert!((19_000..20_480).contains(&length));
    value["Id"] = Value::String("x".repeat(20_480 - length));
    let document = serde_json::to_string(&value).unwrap();
    assert_eq!(document.len(), 20_480);
    document
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "retain scoped policy timing measurements in the owned test record"
)]
fn maximum_documents_bound_expansion_and_evaluate_adversarial_wildcards() {
    let key = "a".repeat(1024);
    let matching_key = format!("{}b", "a".repeat(1023));
    let pattern = format!("{}b", "*a".repeat(490));
    let resource_patterns = vec![format!("arn:aws:s3:::photos/{pattern}"); 20];
    let condition_patterns = vec![pattern; 20];
    let action_patterns = vec!["s3:*"; 2_800];
    let resource = format!("arn:aws:s3:::photos/{key}");
    let matching_resource = format!("arn:aws:s3:::photos/{matching_key}");
    let action = Action::parse("s3:GetObject").unwrap();

    let workloads = [
        (
            "resource-patterns",
            json!({"Effect": "Allow", "Action": "s3:GetObject", "Resource": resource_patterns}),
        ),
        (
            "condition-patterns",
            json!({"Effect": "Allow", "Action": "s3:GetObject", "Resource": "*", "Condition": {
                "StringLike": {"s3:prefix": condition_patterns}
            }}),
        ),
        (
            "action-expansion",
            json!({"Effect": "Allow", "Action": action_patterns, "Resource": "*"}),
        ),
    ];

    for (name, statement) in workloads {
        let document = full_document(&statement);
        let started = Instant::now();
        let policy = Policy::parse(&document, PolicyKind::Identity).unwrap();
        let parse_us = started.elapsed().as_micros();

        assert!(matches!(
            Policy::parse(&(document.clone() + " "), PolicyKind::Identity),
            Err(PolicyError::TooLarge)
        ));

        let mut context = Context::default();
        context.insert("s3:prefix", vec![key.clone()]).unwrap();

        let expected = if name == "action-expansion" {
            Verdict::Allow
        } else {
            // Each candidate requires a final b; neither the key nor prefix has
            // one. Every candidate must be examined before the implicit denial.
            Verdict::ImplicitDeny
        };

        let started = Instant::now();

        for _ in 0..8 {
            assert_eq!(
                policy
                    .evaluate(action, &resource, Some(PRINCIPAL), &context)
                    .unwrap(),
                expected,
                "{name}"
            );
        }

        let evaluate_us = started.elapsed().as_micros();
        let mut matching = Context::default();

        matching
            .insert("s3:prefix", vec![matching_key.clone()])
            .unwrap();

        assert_eq!(
            policy
                .evaluate(action, &matching_resource, Some(PRINCIPAL), &matching)
                .unwrap(),
            Verdict::Allow,
            "{name} matching terminal value"
        );

        if name == "action-expansion" {
            for action in ACTIONS {
                let action = Action::parse(action).unwrap();

                assert_eq!(
                    policy
                        .evaluate(action, &resource, Some(PRINCIPAL), &context)
                        .unwrap(),
                    Verdict::Allow
                );
            }
        }

        println!(
            "policy-measurement {}",
            json!({
                "workload": name,
                "document_bytes": document.len(),
                "key_bytes": key.len(),
                "parse_us": parse_us,
                "evaluations": 8,
                "total_evaluate_us": evaluate_us,
            })
        );
    }
}
