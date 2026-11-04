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

    /// The guest compiler's `rustc -V` line, e.g.
    /// `rustc 1.94.1-dev (06e01cb0d 2026-04-09)`. It names the exact compiler
    /// commit and no host platform, so it is what a `Raster.lock` records: tile
    /// image ids depend on it.
    pub fn rustc_version(&self) -> Result<String> {
        let output = std::process::Command::new(self.rustc())
            .arg("-V")
            .output()
            .map_err(|e| Error::Toolchain(format!("{}: {e}", self.rustc().display())))?;
        if !output.status.success() {
            return Err(Error::Toolchain(format!(
                "{} -V failed: {}",
                self.rustc().display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

/// What selects and identifies the toolchain [`Toolchain::find`] returns. A
/// build script that embeds guest ids must rerun when any of these change, or
/// an incremental build keeps guests from the old compiler while a clean build
/// uses the new one, and their ids disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainInputs {
    /// Environment variables the selection reads.
    pub env: Vec<&'static str>,
    /// Files and directories that select the toolchain (rzup's settings, and
    /// the installed toolchains when no default is set) or identify it (the
    /// compilers themselves).
    pub paths: Vec<PathBuf>,
}

impl Toolchain {
    /// The inputs that chose and identify this toolchain, for
    /// `cargo:rerun-if-env-changed` / `cargo:rerun-if-changed`.
    pub fn inputs(&self) -> ToolchainInputs {
        let explicit = env::var_os("RISC0_RUST_TOOLCHAIN_PATH").filter(|v| !v.is_empty());
        let mut paths = vec![self.rustc(), self.cargo()];
        if let Some(cc) = Self::cc() {
            paths.push(cc);
        }
        // rzup's selection, unless the path is given outright. It also picks
        // the C++ toolchain behind `cc()`, so it is read either way.
        if let Some(risc0_home) = risc0_home() {
            let selected = explicit.is_none().then_some(self.id.as_str());
            paths.extend(rzup_selection_inputs(&risc0_home, selected));
        }
        ToolchainInputs {
            env: vec!["RISC0_RUST_TOOLCHAIN_PATH", "RISC0_HOME", "HOME"],
            paths,
        }
    }
}

/// rzup's home: `$RISC0_HOME`, else `~/.risc0` (rzup's `Environment`).
fn risc0_home() -> Option<PathBuf> {
    env::var_os("RISC0_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".risc0")))
}

/// What rzup's choice of `selected` (the chosen toolchain directory's name, or
/// `None` when `RISC0_RUST_TOOLCHAIN_PATH` chose) depends on, under
/// `risc0_home`. rzup takes `settings.toml`'s `[default_versions] rust` when
/// that version is installed, and otherwise the highest installed version
/// (`Registry::get_default_component_version`). So the settings file always
/// counts; the installed toolchains count too whenever the selection did not
/// come from that default (none set, or the default not installed), since
/// installing another one can then change it. Cargo scans a tracked directory
/// recursively, so the toolchains directory is tracked only then.
fn rzup_selection_inputs(risc0_home: &Path, selected: Option<&str>) -> Vec<PathBuf> {
    let settings = risc0_home.join("settings.toml");
    let mut paths = Vec::new();
    let rust_default = std::fs::read_to_string(&settings)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| {
            table
                .get("default_versions")?
                .get("rust")?
                .as_str()
                .map(str::to_string)
        });
    if settings.is_file() {
        paths.push(settings);
    }
    let from_default = match (selected, rust_default.as_deref()) {
        (Some(id), Some(version)) => id.starts_with(&format!("v{version}-")),
        _ => false,
    };
    if selected.is_some() && !from_default {
        let toolchains = risc0_home.join("toolchains");
        if toolchains.is_dir() {
            paths.push(toolchains);
        }
    }
    paths
}

fn exe(name: &str) -> String {
    format!("{name}{}", env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selection_inputs_follow_rzup() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("toolchains/v1.94.1-rust-test")).unwrap();

        let selected = Some("v1.94.1-rust-test");
        let both = vec![home.join("settings.toml"), home.join("toolchains")];

        // No settings file: rzup took the highest installed toolchain.
        assert_eq!(rzup_selection_inputs(home, selected), vec![home.join("toolchains")]);

        // The selection is the settings file's Rust default: it decides alone.
        std::fs::write(home.join("settings.toml"), "[default_versions]\nrust = \"1.94.1\"\n").unwrap();
        assert_eq!(rzup_selection_inputs(home, selected), vec![home.join("settings.toml")]);

        // A default that is not installed: rzup fell back, so both count.
        std::fs::write(home.join("settings.toml"), "[default_versions]\nrust = \"1.99.0\"\n").unwrap();
        assert_eq!(rzup_selection_inputs(home, selected), both);

        // Settings without a Rust default: both count.
        std::fs::write(home.join("settings.toml"), "[default_versions]\ncpp = \"2024.1.5\"\n").unwrap();
        assert_eq!(rzup_selection_inputs(home, selected), both);

        // RISC0_RUST_TOOLCHAIN_PATH chose: only the settings file (it still
        // picks the C++ toolchain), not the installed Rust toolchains.
        assert_eq!(rzup_selection_inputs(home, None), vec![home.join("settings.toml")]);
    }

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

    #[cfg(unix)]
    #[test]
    fn the_rustc_version_is_its_v_line() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("v9.9.9-rust-test");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/cargo"), "").unwrap();
        let rustc = dir.join("bin/rustc");
        std::fs::write(&rustc, "#!/bin/sh\necho 'rustc 9.9.9-dev (abcdef012 2030-01-01)'\n").unwrap();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let toolchain = Toolchain::from_dir(&dir).unwrap();
        // Executing a file just written can fail with ETXTBSY while a parallel
        // test forks; a real toolchain's rustc is never freshly written.
        let version = (0..50)
            .find_map(|_| match toolchain.rustc_version() {
                Err(e) if e.to_string().contains("busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                }
                other => Some(other),
            })
            .expect("rustc stayed busy")
            .unwrap();
        assert_eq!(version, "rustc 9.9.9-dev (abcdef012 2030-01-01)");
    }
}
