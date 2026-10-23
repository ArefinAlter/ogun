//! The examples and the JSON Schema of O-02 agree with the validator.

use std::path::PathBuf;

use ogun_ir::{Catalogue, check};
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn schema() -> jsonschema::Validator {
    let schema: Value =
        serde_json::from_str(&read("docs/spec/schema/workflow-ir.v0.schema.json")).unwrap();
    jsonschema::validator_for(&schema).expect("schema compiles")
}

const EXAMPLES: &[&str] = &[
    "examples/climate-papers.workflow.json",
    "examples/climate-papers.v18.workflow.json",
    "examples/noaa-daily.workflow.json",
    "examples/air-quality-stream.workflow.json",
];

#[test]
fn examples_are_valid_and_match_the_schema() {
    let schema = schema();
    for rel in EXAMPLES {
        let text = read(rel);
        let report = check(&text, &Catalogue::builtin());
        assert!(report.is_valid(), "{rel}: {:?}", report.diagnostics);
        assert!(
            report.diagnostics.is_empty(),
            "{rel}: {:?}",
            report.diagnostics
        );
        let value: Value = serde_json::from_str(&text).unwrap();
        let errors: Vec<String> = schema.iter_errors(&value).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{rel}: {errors:?}");
    }
}

#[test]
fn the_normal_form_matches_the_schema() {
    let schema = schema();
    for rel in EXAMPLES {
        let report = check(&read(rel), &Catalogue::builtin());
        let normal = ogun_ir::hash::normal_form(report.workflow().unwrap()).unwrap();
        let errors: Vec<String> = schema.iter_errors(&normal).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{rel}: {errors:?}");
    }
}

#[test]
fn v17_to_v18_needs_approval() {
    let a = check(&read(EXAMPLES[0]), &Catalogue::builtin());
    let b = check(&read(EXAMPLES[1]), &Catalogue::builtin());
    let d = ogun_ir::diff(
        a.workflow().unwrap(),
        b.workflow().unwrap(),
        &Catalogue::builtin(),
    );
    assert!(d.needs_approval());
    assert_eq!(d.nodes_added, vec!["flag"]);
}
