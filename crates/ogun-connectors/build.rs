//! Bundles `connectors/catalog/*.connector.json` into one JSON array in `OUT_DIR`.

use std::io::Write as _;
use std::path::PathBuf;

fn main() {
    let catalog = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("..")
        .join("..")
        .join("connectors")
        .join("catalog");
    println!("cargo:rerun-if-changed={}", catalog.display());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&catalog)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with(".connector.json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut out = String::from("[");
    for (i, f) in files.iter().enumerate() {
        println!("cargo:rerun-if-changed={}", f.display());
        let text = std::fs::read_to_string(f).unwrap_or_default();
        if i > 0 {
            out.push(',');
        }
        out.push_str(text.trim());
    }
    out.push(']');
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("catalog.json");
    let mut file = std::fs::File::create(dest).expect("cannot create the catalog bundle");
    file.write_all(out.as_bytes())
        .expect("cannot write the catalog bundle");
}
