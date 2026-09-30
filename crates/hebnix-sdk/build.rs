//! Bakes the req.hebnix.com request key into the build.
//!
//! Read from the HEBNIX_REQ_KEY env var (CI) or a `req_key.txt` in the
//! workspace root (local builds; gitignored). Builds without either still
//! compile and just send no token.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=HEBNIX_REQ_KEY");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let key_file = manifest.join("../../req_key.txt");
    println!("cargo:rerun-if-changed={}", key_file.display());

    let key = env::var("HEBNIX_REQ_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| fs::read_to_string(&key_file).ok())
        .unwrap_or_default();
    let key: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("req_key.rs"), format!("pub const KEY: &str = {key:?};\n"))
        .expect("write embedded request key");
}
