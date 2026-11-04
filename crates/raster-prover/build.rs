//! Builds the protocol guests listed in `[package.metadata.risc0].methods`
//! for the zkVM and writes `methods.rs`: `<NAME>_ELF`, `<NAME>_PATH` and
//! `<NAME>_ID` per guest binary, the constants risc0-build's `embed_methods`
//! wrote.
//!
//! The guests are built with raster-guest-build, each rehomed at the root of a
//! stage, so their image ids do not depend on the checkout path or
//! `$CARGO_HOME` (docs/proposals/path-independent-builds.md §4.4).
//!
//! Outputs are rewritten only when their content changes, and the rerun
//! triggers name exactly what the guests depend on: their sources and local
//! path packages, and what selects and identifies the guest compiler (rzup's
//! default and the compiler binaries, `Toolchain::inputs`). Without the latter,
//! switching rzup's default left an incremental build with the old guests and
//! ids while a clean build used the new compiler. risc0-build registered its
//! own `methods.rs`, rewritten on every run, so this crate used to recompile on
//! every build.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use raster_guest_build::{
    cargo_home, stage_plan, GuestBuild, GuestCrate, PathPackages, Stage, Toolchain,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=Cargo.toml");
    for var in ["RISC0_SKIP_BUILD", "CARGO_HOME"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let guests = guest_dirs(&manifest_dir);
    let skip = env::var_os("RISC0_SKIP_BUILD").is_some_and(|v| !v.is_empty());
    let mut methods = String::new();
    if skip {
        // As risc0-build: an empty ELF and a zero id, so the crate builds where
        // no risc0 toolchain is installed.
        for dir in &guests {
            println!("cargo:rerun-if-changed={}", dir.join("Cargo.toml").display());
            for name in binary_names(dir) {
                methods.push_str(&constants(&name, None, &[0; 8]));
            }
        }
    } else {
        let toolchain = Toolchain::find().unwrap_or_else(|e| panic!("{e}"));
        let inputs = toolchain.inputs();
        for var in &inputs.env {
            println!("cargo:rerun-if-env-changed={var}");
        }
        for path in &inputs.paths {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        let cargo_home = cargo_home().unwrap_or_else(|e| panic!("{e}"));
        let target_dir = guest_target_dir(&out_dir);
        for dir in &guests {
            let packages = PathPackages::resolve(&dir.join("Cargo.toml"))
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            for package in &packages.packages {
                for input in ["Cargo.toml", "src", "build.rs"] {
                    let path = package.dir.join(input);
                    if path.exists() {
                        println!("cargo:rerun-if-changed={}", path.display());
                    }
                }
            }
            if let Some(lockfile) = &packages.lockfile {
                println!("cargo:rerun-if-changed={}", lockfile.display());
            }

            let plan = stage_plan(&packages).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            let stage = Stage::create(plan).unwrap_or_else(|e| panic!("{e}"));
            let output = GuestBuild {
                stage: &stage,
                krate: GuestCrate::Package { dir: dir.clone() },
                lockfile: packages.lockfile.as_deref(),
                toolchain: &toolchain,
                cargo_home: &cargo_home,
                target_dir: Some(&target_dir),
            }
            .run()
            .unwrap_or_else(|e| panic!("building the guest in {}: {e}", dir.display()));

            // Keep the guest's lockfile current, as building the guest directly
            // with cargo would.
            if let Some(lockfile) = &packages.lockfile {
                write_if_changed(lockfile, &output.lockfile);
            }
            for binary in &output.binaries {
                let path = out_dir.join(format!("{}.bin", binary.name));
                write_if_changed(&path, &binary.program);
                methods.push_str(&constants(&binary.name, Some(&path), &binary.image_id_words));
            }
        }
    }
    write_if_changed(&out_dir.join("methods.rs"), methods.as_bytes());
}

/// The guest directories in `[package.metadata.risc0].methods`.
fn guest_dirs(manifest_dir: &Path) -> Vec<PathBuf> {
    let manifest: toml::Table = fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("read Cargo.toml")
        .parse()
        .expect("parse Cargo.toml");
    manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("risc0"))
        .and_then(|r| r.get("methods"))
        .and_then(|m| m.as_array())
        .expect("[package.metadata.risc0].methods")
        .iter()
        .map(|m| manifest_dir.join(m.as_str().expect("a method path")))
        .collect()
}

/// A guest's binary names without building it: its `[[bin]]` names, else its
/// package name (the default binary).
fn binary_names(dir: &Path) -> Vec<String> {
    let manifest: toml::Table = fs::read_to_string(dir.join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .parse()
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let bins: Vec<String> = manifest
        .get("bin")
        .and_then(|b| b.as_array())
        .map(|bins| {
            bins.iter()
                .filter_map(|b| b.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !bins.is_empty() {
        return bins;
    }
    vec![manifest["package"]["name"]
        .as_str()
        .expect("package.name")
        .to_string()]
}

fn constants(name: &str, path: Option<&Path>, id: &[u32; 8]) -> String {
    let upper = name.to_uppercase().replace('-', "_");
    let (elf, path) = match path {
        Some(path) => (format!("include_bytes!({:?})", path), format!("{:?}", path)),
        None => ("&[]".to_string(), "\"\"".to_string()),
    };
    format!(
        "pub const {upper}_ELF: &[u8] = {elf};\n\
         pub const {upper}_PATH: &str = {path};\n\
         pub const {upper}_ID: [u32; 8] = {id:?};\n"
    )
}

/// `<target dir>/riscv-guest/raster-prover`: shared by every guest and every
/// profile, like risc0-build's. The target directory is the nearest ancestor
/// of `OUT_DIR` holding cargo's `CACHEDIR.TAG`.
fn guest_target_dir(out_dir: &Path) -> PathBuf {
    out_dir
        .ancestors()
        .find(|dir| dir.join("CACHEDIR.TAG").is_file())
        .map(|target| target.join("riscv-guest").join("raster-prover"))
        .unwrap_or_else(|| out_dir.join("guest-target"))
}

/// Write only when the content differs, so an unchanged output keeps its
/// mtime and nothing that includes it is rebuilt.
fn write_if_changed(path: &Path, contents: &[u8]) {
    if fs::read(path).ok().as_deref() != Some(contents) {
        fs::write(path, contents).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}
