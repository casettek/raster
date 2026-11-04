//! Rewriting a manifest's local paths for its place inside a stage.
//!
//! A stage renames the raster checkout's entry to [`crate::RASTER_ENTRY`].
//! A manifest outside that entry may still name the checkout by its real name
//! (`../raster-main/crates/raster`), which does not exist inside the stage.
//! [`restage_paths`] rewrites such paths to the same package's staged
//! location, so every package keeps exactly one path, and cargo never sees a
//! second copy of it.

use std::path::{Component, Path, PathBuf};

use crate::guest::path_string;
use crate::stage::{normalize, StagePlan};
use crate::{Error, Result};

const DEPENDENCY_KINDS: [&str; 3] = ["dependencies", "build-dependencies", "dev-dependencies"];

/// Rewrite every relative local path in `manifest`, which was read from the
/// real directory `dir` and sits at `staged_dir` (relative to the stage root),
/// to point at the staged location of what it named:
///
/// - `path` in `[dependencies]`, `[build-dependencies]`, `[dev-dependencies]`,
///   their `[target.*]` forms, `[patch.*]` and `[workspace.dependencies]`;
/// - `package.workspace`.
///
/// Absolute paths are left as written: they name a real location, and the
/// stage leaves it alone. A relative path that leaves every staged entry is an
/// error when `strict`, and is otherwise left as written (a dev-dependency the
/// guest never loads, say). Returns whether anything changed.
pub(crate) fn restage_paths(
    manifest: &mut toml::Table,
    dir: &Path,
    staged_dir: &Path,
    plan: &StagePlan,
    strict: bool,
) -> Result<bool> {
    let restage = |path: &str, what: &str| -> Result<Option<String>> {
        if Path::new(path).is_absolute() {
            return Ok(None);
        }
        let real = normalize(&dir.join(path));
        let staged = match plan.stage_relative(&real) {
            Ok(staged) => staged,
            Err(e) if strict => {
                return Err(Error::Layout(format!("{what} ({}): {e}", real.display())))
            }
            Err(_) => return Ok(None),
        };
        let rewritten = path_string(&relative_path(staged_dir, &staged))?;
        Ok((rewritten != path).then_some(rewritten))
    };
    let mut changed = false;
    let mut rewrite_table = |table: &mut toml::Table| -> Result<()> {
        for (name, dependency) in table.iter_mut() {
            let Some(dependency) = dependency.as_table_mut() else {
                continue;
            };
            let Some(path) = dependency.get("path").and_then(|p| p.as_str()) else {
                continue;
            };
            if let Some(rewritten) = restage(path, &format!("dependency `{name}`"))? {
                dependency.insert("path".into(), rewritten.into());
                changed = true;
            }
        }
        Ok(())
    };

    for kind in DEPENDENCY_KINDS {
        if let Some(table) = manifest.get_mut(kind).and_then(|t| t.as_table_mut()) {
            rewrite_table(table)?;
        }
    }
    if let Some(targets) = manifest.get_mut("target").and_then(|t| t.as_table_mut()) {
        for (_, target) in targets.iter_mut() {
            for kind in DEPENDENCY_KINDS {
                if let Some(table) = target.get_mut(kind).and_then(|t| t.as_table_mut()) {
                    rewrite_table(table)?;
                }
            }
        }
    }
    if let Some(patches) = manifest.get_mut("patch").and_then(|t| t.as_table_mut()) {
        for (_, source) in patches.iter_mut() {
            if let Some(table) = source.as_table_mut() {
                rewrite_table(table)?;
            }
        }
    }
    if let Some(table) = manifest
        .get_mut("workspace")
        .and_then(|w| w.get_mut("dependencies"))
        .and_then(|d| d.as_table_mut())
    {
        rewrite_table(table)?;
    }
    if let Some(package) = manifest.get_mut("package").and_then(|p| p.as_table_mut()) {
        if let Some(path) = package.get("workspace").and_then(|w| w.as_str()) {
            if let Some(rewritten) = restage(path, "package.workspace")? {
                package.insert("workspace".into(), rewritten.into());
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// The path from directory `from` to `to`, both relative to the same root,
/// e.g. `app` → `raster/crates/raster` is `../raster/crates/raster`.
pub(crate) fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path: PathBuf = std::iter::repeat_n(Component::ParentDir, from.len() - common).collect();
    path.extend(&to[common..]);
    if path.as_os_str().is_empty() {
        path.push(".");
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative_path(&p("app"), &p("raster/crates/raster")), p("../raster/crates/raster"));
        assert_eq!(
            relative_path(&p("iris/raster-stages/x"), &p("raster/crates/raster")),
            p("../../../raster/crates/raster")
        );
        assert_eq!(relative_path(&p(""), &p("raster/crates/raster-core")), p("raster/crates/raster-core"));
        assert_eq!(relative_path(&p("a/b"), &p("a/b/c")), p("c"));
        assert_eq!(relative_path(&p("a/b"), &p("a/b")), p("."));
    }

    /// A sibling app naming the checkout `raster-main`, staged as `raster`.
    fn plan() -> StagePlan {
        let raster = p("/p/raster-main");
        StagePlan::new(&[p("/p/app"), raster.clone()], Some(&raster)).unwrap()
    }

    fn rewrite(text: &str) -> (bool, toml::Table) {
        let mut manifest: toml::Table = text.parse().unwrap();
        let changed = restage_paths(&mut manifest, Path::new("/p/app"), Path::new("app"), &plan(), false).unwrap();
        (changed, manifest)
    }

    #[test]
    fn a_renamed_checkout_is_renamed_in_every_path_form() {
        let (changed, manifest) = rewrite(
            r#"
            [package]
            name = "app"
            workspace = "../raster-main"
            [dependencies]
            raster = { path = "../raster-main/crates/raster", default-features = false }
            local = { path = "crates/local" }
            [target.'cfg(unix)'.build-dependencies]
            core = { path = "../raster-main/crates/raster-core" }
            [patch.crates-io]
            macros = { path = "../raster-main/crates/raster-macros" }
            [workspace.dependencies]
            raster = { path = "../raster-main/crates/raster" }
            "#,
        );
        assert!(changed);
        assert_eq!(manifest["dependencies"]["raster"]["path"].as_str(), Some("../raster/crates/raster"));
        assert_eq!(manifest["dependencies"]["raster"]["default-features"].as_bool(), Some(false));
        assert_eq!(manifest["dependencies"]["local"]["path"].as_str(), Some("crates/local"), "unchanged");
        assert_eq!(
            manifest["target"]["cfg(unix)"]["build-dependencies"]["core"]["path"].as_str(),
            Some("../raster/crates/raster-core")
        );
        assert_eq!(
            manifest["patch"]["crates-io"]["macros"]["path"].as_str(),
            Some("../raster/crates/raster-macros")
        );
        assert_eq!(
            manifest["workspace"]["dependencies"]["raster"]["path"].as_str(),
            Some("../raster/crates/raster")
        );
        assert_eq!(manifest["package"]["workspace"].as_str(), Some("../raster"));
    }

    #[test]
    fn paths_that_stay_valid_are_untouched() {
        let (changed, _) = rewrite(
            r#"
            [dependencies]
            local = { path = "crates/local" }
            absolute = { path = "/somewhere/else/crate" }
            outside = { path = "../../not-staged" }
            serde = "1"
            "#,
        );
        assert!(!changed, "nothing names the renamed checkout");
    }

    #[test]
    fn strict_mode_rejects_a_path_outside_the_stage() {
        let mut manifest: toml::Table = r#"
            [dependencies]
            outside = { path = "../../not-staged" }
        "#
        .parse()
        .unwrap();
        let error = restage_paths(&mut manifest, Path::new("/p/app"), Path::new(""), &plan(), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("dependency `outside`"), "{error}");
    }
}
