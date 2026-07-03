//! Corpus replay (M1-T21): every `.ron` vector under `vectors/` runs on the
//! interpreter and must match its stored expectation. Vectors are self-contained
//! (the oracle's truth is baked in), so this needs no Unicorn — it's the
//! permanent regression suite the `unicorn` feature generates.

use std::fs;
use std::path::{Path, PathBuf};

use x86jit_tests::compare::check;
use x86jit_tests::oracle::{InterpreterOracle, Oracle, VectorInput};
use x86jit_tests::vector::TestVector;

fn collect_ron(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_ron(&path, out);
        } else if path.extension().is_some_and(|e| e == "ron") {
            out.push(path);
        }
    }
}

#[test]
fn every_vector_matches_the_interpreter() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors");
    let mut files = Vec::new();
    collect_ron(&root, &mut files);
    assert!(!files.is_empty(), "no vectors found under {}", root.display());

    let mut failures = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        let vector = TestVector::from_ron(&text)
            .unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));

        let input = VectorInput {
            cpu_init: vector.cpu_init.clone(),
            mem_init: vector.mem_init.clone(),
            entry: vector.entry,
            run: vector.run,
        };
        let outcome = InterpreterOracle.run(&input);

        if let Some(div) = check(&vector, &outcome) {
            failures.push(format!("FAIL {}\n{div}", vector.name));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} vectors diverged:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}
