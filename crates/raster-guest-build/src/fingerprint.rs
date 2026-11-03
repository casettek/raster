//! A cache key over everything a guest compiles.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::recipe::RECIPE_VERSION;
use crate::Result;

/// How much of a package a guest compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A dependency: the guest compiles its library only. `src/main.rs` and
    /// `src/bin/` are left out, so editing, say, a project's sequences in
    /// `main.rs` does not rebuild its tile guests.
    Library,
    /// The guest package itself: everything under `src/`.
    Whole,
}

/// A hash of every input to a guest build.
///
/// Each package's files are recorded under a caller-chosen label plus their
/// path inside the package, never under their absolute path. With
/// stage-relative labels (see [`crate::Stage::relative`]) the same sources at
/// two checkout paths give the same fingerprint, like the image id itself.
pub struct Fingerprint {
    hasher: Sha256,
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self::new()
    }
}

impl Fingerprint {
    /// A fingerprint seeded with [`RECIPE_VERSION`].
    pub fn new() -> Self {
        let mut fingerprint = Self {
            hasher: Sha256::new(),
        };
        fingerprint.bytes("recipe", &RECIPE_VERSION.to_le_bytes());
        fingerprint
    }

    /// Record `data` under `label`.
    pub fn bytes(&mut self, label: &str, data: &[u8]) {
        for part in [label.as_bytes(), data] {
            self.hasher.update((part.len() as u64).to_le_bytes());
            self.hasher.update(part);
        }
    }

    /// Record a package's `Cargo.toml`, `build.rs` and `src/` (narrowed by
    /// `scope`). Missing files are skipped; a `target/` or hidden directory
    /// under `src/` is never read.
    pub fn package(&mut self, label: &str, dir: &Path, scope: Scope) -> Result<()> {
        let mut files: Vec<PathBuf> = ["Cargo.toml", "build.rs"]
            .iter()
            .map(PathBuf::from)
            .filter(|f| dir.join(f).is_file())
            .collect();
        collect(dir, Path::new("src"), &mut files)?;
        if scope == Scope::Library {
            files.retain(|f| f != Path::new("src/main.rs") && !f.starts_with("src/bin"));
        }
        files.sort();
        for file in files {
            let name = format!("{label}/{}", slash(&file));
            self.bytes(&name, &fs::read(dir.join(&file))?);
        }
        Ok(())
    }

    pub fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }

    pub fn finish_hex(self) -> String {
        hex::encode(self.finish())
    }
}

/// Every file under `dir/relative`, as paths relative to `dir`.
fn collect(dir: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let path = dir.join(relative);
    if path.is_file() {
        out.push(relative.to_path_buf());
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(&path) else {
        return Ok(());
    };
    for entry in entries {
        let name = entry?.file_name();
        if name == "target" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        collect(dir, &relative.join(name), out)?;
    }
    Ok(())
}

/// The same name on every platform.
fn slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A tile project and a raster checkout, as a fingerprint sees them.
    fn layout(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let core = root.join("raster/crates/raster-core");
        let user = root.join("project");
        let extra = root.join("project/crates/det-num");
        for (dir, lib) in [(&core, "// core"), (&user, "// tiles"), (&extra, "// det")] {
            write(&dir.join("Cargo.toml"), "[package]");
            write(&dir.join("src/lib.rs"), lib);
        }
        write(&user.join("src/helpers/mod.rs"), "// helpers");
        write(&user.join("src/main.rs"), "// sequences");
        write(&user.join("src/bin/tool.rs"), "// a tool");
        (core, user, extra)
    }

    fn key(core: &Path, user: &Path, extra: &Path) -> [u8; 32] {
        let mut f = Fingerprint::new();
        f.package("raster/crates/raster-core", core, Scope::Library).unwrap();
        f.package("project", user, Scope::Library).unwrap();
        f.package("project/crates/det-num", extra, Scope::Library).unwrap();
        f.finish()
    }

    #[test]
    fn the_key_tracks_what_a_guest_compiles() {
        let tmp = tempfile::tempdir().unwrap();
        let (core, user, extra) = layout(tmp.path());
        let base = key(&core, &user, &extra);
        assert_eq!(key(&core, &user, &extra), base, "deterministic");

        let changed = |file: PathBuf| {
            let original = fs::read_to_string(&file).unwrap();
            write(&file, &format!("{original}\n// edited"));
            let after = key(&core, &user, &extra);
            write(&file, &original);
            after != base
        };
        assert!(changed(core.join("src/lib.rs")));
        assert!(changed(user.join("src/helpers/mod.rs")), "a helper outside the tile's file");
        assert!(changed(user.join("Cargo.toml")));
        assert!(changed(extra.join("src/lib.rs")), "a local crate beyond raster's three");
        assert!(!changed(user.join("src/main.rs")), "sequences are not compiled");
        assert!(!changed(user.join("src/bin/tool.rs")));
    }

    #[test]
    fn the_key_does_not_depend_on_where_the_sources_live() {
        let tmp = tempfile::tempdir().unwrap();
        let (a_core, a_user, a_extra) = layout(&tmp.path().join("a"));
        let (b_core, b_user, b_extra) = layout(&tmp.path().join("b/much/deeper"));
        assert_eq!(key(&a_core, &a_user, &a_extra), key(&b_core, &b_user, &b_extra));
    }

    #[test]
    fn whole_scope_includes_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, user, _) = layout(tmp.path());
        let digest = |scope| {
            let mut f = Fingerprint::new();
            f.package("p", &user, scope).unwrap();
            f.finish()
        };
        let before = digest(Scope::Whole);
        write(&user.join("src/main.rs"), "// changed");
        assert_ne!(digest(Scope::Whole), before);
    }
}
