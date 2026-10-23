//! The transition table in O-01 section 5.2 and `TRANSITIONS` are the same table.

use ogun_run::TRANSITIONS;

fn name<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn prose_table_matches_code() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/spec/O-01-domain-model-and-run-state-machine.md"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let start = text
        .find("<!-- transitions:begin -->")
        .expect("begin marker");
    let end = text.find("<!-- transitions:end -->").expect("end marker");
    let prose: Vec<[String; 4]> = text[start..end]
        .lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            let cells: Vec<String> = l
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().trim_matches('`').to_owned())
                .collect();
            [
                cells[0].clone(),
                cells[1].clone(),
                cells[2].clone(),
                cells[3].clone(),
            ]
        })
        .collect();
    let code: Vec<[String; 4]> = TRANSITIONS
        .iter()
        .map(|t| [name(t.from), name(t.cause), name(t.to), name(t.actor)])
        .collect();
    assert_eq!(prose, code);
}
