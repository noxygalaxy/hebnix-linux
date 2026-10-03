// Bakes every locales/<code>/*.ftl and the Headscale key into the binary.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    embed_locales();
    embed_headscale_key();
}

/// Bakes the Headscale pre-auth key for Workshop multiplayer into the build.
/// Read from the HEBNIX_HS_KEY env var (CI) or `hs_key.txt` in the workspace
/// root (local builds; gitignored). Without either the build still works, but
/// multiplayer can't connect (see `multiplayer_lan::tailnet_auth_key`).
fn embed_headscale_key() {
    let key_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hs_key.txt");
    println!("cargo:rerun-if-changed={}", key_file.display());
    println!("cargo:rerun-if-env-changed=HEBNIX_HS_KEY");
    let key = std::env::var("HEBNIX_HS_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::fs::read_to_string(&key_file).ok())
        .unwrap_or_default();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(
        out.join("hs_key.rs"),
        format!("pub const KEY: &str = {:?};\n", key.trim()),
    )
    .expect("write embedded Headscale key");
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
