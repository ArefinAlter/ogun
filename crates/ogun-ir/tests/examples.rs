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

/// Every `*.workflow.json` under `examples/`, sorted.
fn examples() -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.to_string_lossy().ends_with(".workflow.json") {
                out.push(
                    path.strip_prefix(root())
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(&root().join("examples"), &mut out);
    out.sort();
    out
}

#[test]
fn examples_are_valid_and_match_the_schema() {
    let schema = schema();
    for rel in &examples() {
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
    for rel in &examples() {
        let report = check(&read(rel), &Catalogue::builtin());
        let normal = ogun_ir::hash::normal_form(report.workflow().unwrap()).unwrap();
        let errors: Vec<String> = schema.iter_errors(&normal).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{rel}: {errors:?}");
    }
}

#[test]
fn v17_to_v18_needs_approval() {
    let a = check(
        &read("examples/climate-papers.workflow.json"),
        &Catalogue::builtin(),
    );
    let b = check(
        &read("examples/climate-papers.v18.workflow.json"),
        &Catalogue::builtin(),
    );
    let d = ogun_ir::diff(
        a.workflow().unwrap(),
        b.workflow().unwrap(),
        &Catalogue::builtin(),
    );
    assert!(d.needs_approval());
    assert_eq!(d.nodes_added, vec!["flag"]);
}

#[test]
fn there_are_examples() {
    assert!(examples().len() >= 9, "{:?}", examples());
}

/// A code node's `code.blake3` names exactly the bytes of the file at `code.path`. If someone edits
/// the script, the hash in the workflow must change too, and with it the workflow hash and the
/// approval it needs.
#[test]
fn example_code_hashes_match_their_files() {
    let mut checked = 0;
    for rel in &examples() {
        let report = check(&read(rel), &Catalogue::builtin());
        for node in &report.workflow().unwrap().nodes {
            if let Some(code) = &node.code {
                if let Some(path) = &code.path {
                    let bytes = std::fs::read(root().join(path))
                        .unwrap_or_else(|e| panic!("{rel}: {path}: {e}"));
                    assert_eq!(
                        blake3::hash(&bytes).to_hex().as_str(),
                        code.blake3,
                        "{rel}: {path}"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 5);
}
