//! The same tile guest, built from two checkouts at different paths, has the
//! same image id. Automates docs/proposals/path-independent-builds.md §3.
//!
//! Needs the risc0 Rust toolchain and builds two guests (about a minute):
//! `cargo test -p raster-guest-build --test two_checkouts -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use raster_guest_build::{
    cargo_home, stage_plan, Dependency, GuestBuild, GuestCrate, PathPackages, Stage, Toolchain,
};

const TILE: &str = "advance_line_cursor";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Copy the repository's tracked and new files, plus its (ignored) Cargo.lock,
/// so both copies resolve the same versions.
fn copy_checkout(repository: &Path, destination: &Path) {
    let listed = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(listed.status.success());
    let files = listed
        .stdout
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| PathBuf::from(String::from_utf8(f.to_vec()).unwrap()))
        .chain([PathBuf::from("Cargo.lock")]);
    for file in files {
        let from = repository.join(&file);
        if !from.is_file() {
            continue; // listed but deleted in the worktree
        }
        let to = destination.join(&file);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(from, to).unwrap();
    }
}

/// GuestBuilder's tile wrapper (raster-backend-risc0's `generate_guest_main`).
fn main_rs(tile: &str) -> String {
    format!(
        r#"#![no_main]
#![no_std]
extern crate alloc;
use hello_tiles::__raster_tile_replay_entry_{tile};
risc0_zkvm::guest::entry!(main);
fn main() {{
    let input_len: u32 = risc0_zkvm::guest::env::read();
    let mut input = alloc::vec![0u8; input_len as usize];
    risc0_zkvm::guest::env::read_slice(&mut input);
    let output = __raster_tile_replay_entry_{tile}(&input).expect("Tile runtime failure");
    risc0_zkvm::guest::env::commit_slice(&output);
}}
"#
    )
}

struct Built {
    image_id: [u8; 32],
    image_id_words: [u32; 8],
    elf: Vec<u8>,
}

fn build_tile(checkout: &Path, toolchain: &Toolchain, cargo_home: &Path) -> Built {
    let packages =
        PathPackages::resolve(&checkout.join("examples/hello-tiles/Cargo.toml")).unwrap();
    let raster = packages.require("raster").unwrap();
    let stage = Stage::create(stage_plan(&packages).unwrap()).unwrap();
    let output = GuestBuild {
        stage: &stage,
        krate: GuestCrate::Generated {
            package: format!("raster-guest-{TILE}"),
            dependencies: vec![
                Dependency::version("risc0-zkvm", "3.0"),
                Dependency::path("raster", &raster.dir).no_default_features(),
                Dependency::path("hello-tiles", &packages.root.dir).no_default_features(),
            ],
            main_rs: main_rs(TILE),
        },
        lockfile: packages.lockfile.as_deref(),
        toolchain,
        cargo_home,
        target_dir: None,
    }
    .run()
    .unwrap();
    assert_eq!(output.binaries.len(), 1);
    let binary = output.binaries.into_iter().next().unwrap();
    assert_eq!(binary.name, format!("raster-guest-{TILE}"));
    Built {
        image_id: binary.image_id,
        image_id_words: binary.image_id_words,
        elf: binary.elf,
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
#[ignore = "builds two real guests with the risc0 toolchain (~1 min)"]
fn a_tile_guest_has_the_same_image_id_from_two_checkout_paths() {
    let toolchain = Toolchain::find().unwrap();
    let cargo_home = cargo_home().unwrap();
    let repository = repository();
    let tmp = tempfile::tempdir().unwrap();
    // Different depths and a different checkout directory name.
    let a = tmp.path().join("a/raster");
    let b = tmp.path().join("b/much/deeper/defender-raster");
    copy_checkout(&repository, &a);
    copy_checkout(&repository, &b);

    let built_a = build_tile(&a, &toolchain, &cargo_home);
    let built_b = build_tile(&b, &toolchain, &cargo_home);

    assert_eq!(
        hex(&built_a.image_id),
        hex(&built_b.image_id),
        "the image id depends on the checkout path"
    );
    for (built, checkout) in [(&built_a, &a), (&built_b, &b)] {
        let checkout = checkout.to_str().unwrap();
        assert!(!contains(&built.elf, checkout), "the ELF names {checkout}");
        assert!(
            !contains(&built.elf, cargo_home.to_str().unwrap()),
            "the ELF names $CARGO_HOME"
        );
        let from_words: Vec<u8> = built
            .image_id_words
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert_eq!(from_words, built.image_id, "the two id encodings disagree");
    }
    println!("image id {}", hex(&built_a.image_id));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
