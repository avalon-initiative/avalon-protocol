//! Generates the table of embedded migrations from `db/migrations/`.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("db/migrations");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();

    let mut out = String::from("pub(crate) const EMBEDDED: &[(&str, &str, &str)] = &[\n");
    for n in &names {
        let base = dir.join(n);
        writeln!(
            out,
            "    ({n:?}, include_str!({up:?}), include_str!({down:?})),",
            up = base.join("up.sql").to_str().unwrap(),
            down = base.join("down.sql").to_str().unwrap(),
        )
        .unwrap();
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("embedded_migrations.rs");
    std::fs::write(dest, out).unwrap();
}
