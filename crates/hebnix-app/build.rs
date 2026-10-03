// Bakes every locales/<code>/*.ftl into the binary (see src/i18n).

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    embed_locales();
}

/// bakes every locales/<code>/*.ftl into the exe. adding a language is just
/// adding a folder, nothing in the rust code needs to change.
fn embed_locales() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut out = String::from("pub const EMBEDDED_FTL: &[(&str, &str)] = &[\n");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let Some(code) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ftl"))
            .collect();
        files.sort();
        for file in files {
            println!("cargo:rerun-if-changed={}", file.display());
            out.push_str(&format!(
                "    ({:?}, include_str!({:?})),\n",
                code,
                file.to_string_lossy()
            ));
        }
    }
    out.push_str("];\n");

    let out_path = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("locales_embedded.rs");
    std::fs::write(out_path, out).expect("failed to write locales_embedded.rs");
}
