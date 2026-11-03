//! Compiler flags and toolchain for a guest build.

use std::env;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Version of the guest build recipe. Folded into every [`crate::Fingerprint`];
/// bump it for a change the fingerprint cannot see, such as a flag or
/// toolchain-selection change.
///
/// 4: staged, path-independent builds (docs/proposals/path-independent-builds.md).
/// Versions 1–3 were `GUEST_BUILD_ABI` in raster-backend-risc0.
pub const RECIPE_VERSION: u32 = 4;

/// The risc0 guest target.
pub const TARGET: &str = "riscv32im-risc0-zkvm-elf";

/// What `$CARGO_HOME` is remapped to in guest binaries.
pub const CARGO_HOME_REMAP: &str = "/cargo";

/// `$CARGO_HOME`, or the default `~/.cargo`, as an absolute path.
///
/// The remap only matches the path cargo actually uses, so the build must run
/// with exactly this value (see [`crate::GuestBuild::run`]).
pub fn cargo_home() -> Result<PathBuf> {
    let home = match env::var_os("CARGO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".cargo"))
            .ok_or_else(|| Error::Toolchain("neither CARGO_HOME nor HOME is set".into()))?,
    };
    Ok(if home.is_absolute() {
        home
    } else {
        env::current_dir()?.join(home)
    })
}

/// The guest rustflags.
///
/// The first group mirrors risc0-build 3.0's private `encode_rust_flags` for a
/// user (non-kernel) guest; keep it in sync on a risc0 bump. The last flag
/// rewrites `$CARGO_HOME` in panic locations. No other remap is needed: in a
/// [`crate::Stage`] every path package is seen by a relative path.
pub fn rustflags(cargo_home: &Path) -> Vec<String> {
    vec![
        // The guest is single-threaded: replace atomics with plain ops.
        "-C".into(),
        "passes=lower-atomic".into(),
        "-C".into(),
        format!(
            "link-arg=-Ttext={:#010x}",
            risc0_zkvm_platform::memory::TEXT_START
        ),
        // A missing entry point is only a linker warning otherwise.
        "-C".into(),
        "link-arg=--fatal-warnings".into(),
        "-C".into(),
        "panic=abort".into(),
        // getrandom 0.3 picks its backend by cfg; risc0-zkvm provides `custom`.
        "--cfg".into(),
        "getrandom_backend=\"custom\"".into(),
        format!(
            "--remap-path-prefix={}={CARGO_HOME_REMAP}",
            cargo_home.display()
        ),
    ]
}

/// `flags` as `CARGO_ENCODED_RUSTFLAGS`.
pub fn encode_rustflags(flags: &[String]) -> String {
    flags.join("\x1f")
}

/// A risc0 Rust toolchain.
///
/// Both its rustc and its cargo build the guest. cargo's version matters as
/// well as rustc's: cargo computes the `-C metadata` hashes the code layout
/// follows. (risc0-build uses whatever `cargo` is on `PATH`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// The toolchain directory's name, e.g. `v1.94.1-rust-x86_64-unknown-linux-gnu`.
    pub id: String,
    pub dir: PathBuf,
}

impl Toolchain {
    /// `RISC0_RUST_TOOLCHAIN_PATH` if set, else rzup's default Rust toolchain:
    /// the one risc0-build uses. Not "the newest directory", which differs by
    /// what happens to be installed.
    pub fn find() -> Result<Self> {
        if let Some(path) = env::var_os("RISC0_RUST_TOOLCHAIN_PATH").filter(|v| !v.is_empty()) {
            return Self::from_dir(Path::new(&path));
        }
        let rzup = rzup::Rzup::new().map_err(|e| Error::Toolchain(format!("rzup: {e}")))?;
        let (_, dir) = rzup
            .get_default_version(&rzup::Component::RustToolchain)
            .map_err(|e| Error::Toolchain(format!("rzup: {e}")))?
            .ok_or_else(|| {
                Error::Toolchain(
                    "no default risc0 Rust toolchain; install one with `rzup install rust`".into(),
                )
            })?;
        Self::from_dir(&dir)
    }

    /// The toolchain installed in `dir`.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let toolchain = Self {
            id: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| Error::Toolchain(format!("{} has no name", dir.display())))?,
            dir: dir.to_path_buf(),
        };
        for tool in [toolchain.cargo(), toolchain.rustc()] {
            if !tool.is_file() {
                return Err(Error::Toolchain(format!("{} not found", tool.display())));
            }
        }
        Ok(toolchain)
    }

    pub fn cargo(&self) -> PathBuf {
        self.dir.join("bin").join(exe("cargo"))
    }

    pub fn rustc(&self) -> PathBuf {
        self.dir.join("bin").join(exe("rustc"))
    }

    /// rzup's default C++ toolchain compiler, as risc0-build sets `CC`.
    pub fn cc() -> Option<PathBuf> {
        let rzup = rzup::Rzup::new().ok()?;
        let (_, dir) = rzup
            .get_default_version(&rzup::Component::CppToolchain)
            .ok()??;
        Some(dir.join("bin").join(exe("riscv32-unknown-elf-gcc"))).filter(|cc| cc.is_file())
    }
}

fn exe(name: &str) -> String {
    format!("{name}{}", env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustflags_remap_cargo_home_last() {
        let flags = rustflags(Path::new("/home/someone/.cargo"));
        assert_eq!(
            flags.last().unwrap(),
            "--remap-path-prefix=/home/someone/.cargo=/cargo"
        );
        assert!(flags.contains(&"link-arg=-Ttext=0x00200800".to_string()));
        assert_eq!(
            encode_rustflags(&["-C".into(), "panic=abort".into()]),
            "-C\x1fpanic=abort"
        );
    }

    #[test]
    fn a_toolchain_dir_must_hold_cargo_and_rustc() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("v9.9.9-rust-test");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        assert!(Toolchain::from_dir(&dir).is_err());
        std::fs::write(dir.join("bin").join(exe("cargo")), "").unwrap();
        std::fs::write(dir.join("bin").join(exe("rustc")), "").unwrap();
        let toolchain = Toolchain::from_dir(&dir).unwrap();
        assert_eq!(toolchain.id, "v9.9.9-rust-test");
    }
}
