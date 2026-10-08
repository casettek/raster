//! Every SHA-256 in raster-core goes through [`raster_core::hash`].
//!
//! Inside a guest, `sha2::Sha256` runs as plain RV32 code; `hash::Sha256` is
//! RISC Zero's accelerated implementation there. On the host the two are the
//! same type, so neither the compiler nor clippy can tell a stray `sha2::` use
//! apart — this scan of the sources does. Also covers raster-macros, whose
//! generated tile code is compiled into every tile guest.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn sha256_only_through_hash_module() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let hash_module = manifest.join("src/hash.rs");
    let mut files = Vec::new();
    rust_files(&manifest.join("src"), &mut files);
    rust_files(&manifest.join("../raster-macros/src"), &mut files);

    let mut offenders = Vec::new();
    for file in files.iter().filter(|file| **file != hash_module) {
        let source = fs::read_to_string(file).unwrap();
        for (index, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap();
            if code.contains("sha2::") {
                offenders.push(format!("{}:{}: {}", file.display(), index + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use raster_core::hash::{{Digest, Sha256}} instead of sha2 (not accelerated in the zkVM):\n{}",
        offenders.join("\n")
    );
}
