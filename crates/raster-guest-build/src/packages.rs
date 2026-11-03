//! The local path packages a guest compiles, as cargo resolves them.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use cargo_metadata::{DependencyKind, MetadataCommand, Package, PackageId};

use crate::stage::normalize;
use crate::{Error, Result};

/// A package cargo resolves from a local path (`source: null` in
/// `cargo metadata`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPackage {
    pub name: String,
    pub version: String,
    /// The package directory as cargo reports it: absolute and lexically
    /// normalized, but never canonicalized. A symlink in the path is kept, as
    /// cargo keeps it; resolving it would name the package by a second path
    /// and make cargo see two packages.
    pub dir: PathBuf,
    /// Root of the workspace the package belongs to, or its own directory when
    /// it belongs to none.
    pub workspace_root: PathBuf,
}

/// The path packages reachable from one root package.
#[derive(Debug, Clone)]
pub struct PathPackages {
    /// The package `cargo metadata` was pointed at.
    pub root: PathPackage,
    /// Every path package reachable from `root` through normal and build
    /// dependencies, `root` included, sorted by name then directory.
    pub packages: Vec<PathPackage>,
    /// The `Cargo.lock` of the root's workspace, if it exists.
    pub lockfile: Option<PathBuf>,
}

impl PathPackages {
    /// Resolve the path packages of the package whose manifest is
    /// `manifest_path`, with cargo's full dependency resolve.
    ///
    /// Dev-dependencies are not followed: a guest never compiles them.
    pub fn resolve(manifest_path: &Path) -> Result<Self> {
        let metadata = MetadataCommand::new()
            .manifest_path(manifest_path)
            .exec()
            .map_err(|e| Error::Metadata(format!("{}: {e}", manifest_path.display())))?;
        let resolve = metadata.resolve.as_ref().ok_or_else(|| {
            Error::Metadata(format!("{}: no dependency resolve", manifest_path.display()))
        })?;
        let root_id = resolve.root.as_ref().ok_or_else(|| {
            Error::Metadata(format!(
                "{} is a virtual manifest; point at a package",
                manifest_path.display()
            ))
        })?;

        let packages: BTreeMap<&PackageId, &Package> =
            metadata.packages.iter().map(|p| (&p.id, p)).collect();
        let nodes: BTreeMap<&PackageId, _> = resolve.nodes.iter().map(|n| (&n.id, n)).collect();
        let members: HashSet<&PackageId> = metadata.workspace_members.iter().collect();
        let metadata_root = normalize(metadata.workspace_root.as_std_path());

        let mut seen: HashSet<&PackageId> = HashSet::new();
        let mut queue: VecDeque<&PackageId> = VecDeque::from([root_id]);
        let mut found = Vec::new();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            let package = packages.get(id).ok_or_else(|| {
                Error::Metadata(format!("package {id} is in the resolve but not listed"))
            })?;
            if package.source.is_none() {
                let dir = package_dir(package)?;
                let workspace_root = if members.contains(id) {
                    metadata_root.clone()
                } else {
                    find_workspace_root(&dir)?
                };
                found.push(PathPackage {
                    name: package.name.clone(),
                    version: package.version.to_string(),
                    dir,
                    workspace_root,
                });
            }
            if let Some(node) = nodes.get(id) {
                for dep in &node.deps {
                    let compiled = dep.dep_kinds.is_empty()
                        || dep.dep_kinds.iter().any(|k| {
                            matches!(k.kind, DependencyKind::Normal | DependencyKind::Build)
                        });
                    if compiled {
                        queue.push_back(&dep.pkg);
                    }
                }
            }
        }

        let root = found
            .iter()
            .find(|p| {
                packages
                    .get(root_id)
                    .is_some_and(|r| r.name == p.name && package_dir(r).ok().as_ref() == Some(&p.dir))
            })
            .cloned()
            .ok_or_else(|| {
                Error::Metadata(format!(
                    "{}: the root package is not a path package",
                    manifest_path.display()
                ))
            })?;
        found.sort_by(|a, b| (&a.name, &a.dir).cmp(&(&b.name, &b.dir)));
        let lockfile = Some(metadata_root.join("Cargo.lock")).filter(|p| p.is_file());
        Ok(Self {
            root,
            packages: found,
            lockfile,
        })
    }

    /// The path package named `name`, if the graph has exactly one.
    pub fn get(&self, name: &str) -> Option<&PathPackage> {
        let mut matching = self.packages.iter().filter(|p| p.name == name);
        let first = matching.next()?;
        matching.next().is_none().then_some(first)
    }

    /// The path package named `name`, or an error naming the root package.
    pub fn require(&self, name: &str) -> Result<&PathPackage> {
        let count = self.packages.iter().filter(|p| p.name == name).count();
        match count {
            1 => Ok(self.get(name).expect("counted one")),
            0 => Err(Error::Metadata(format!(
                "no `{name}` path package in the dependency graph of `{}` ({}); \
                 a guest needs `{name}` as a local path dependency",
                self.root.name,
                self.root.dir.display()
            ))),
            _ => Err(Error::Metadata(format!(
                "`{}` reaches {count} different `{name}` path packages",
                self.root.name
            ))),
        }
    }

    /// Every workspace root the path packages belong to, deduplicated.
    pub fn workspace_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self
            .packages
            .iter()
            .map(|p| p.workspace_root.clone())
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }
}

fn package_dir(package: &Package) -> Result<PathBuf> {
    package
        .manifest_path
        .parent()
        .map(|dir| normalize(dir.as_std_path()))
        .ok_or_else(|| Error::Metadata(format!("{}: manifest has no directory", package.name)))
}

fn read_manifest(path: &Path) -> Result<toml::Table> {
    let text = fs::read_to_string(path)?;
    text.parse::<toml::Table>()
        .map_err(|e| Error::Metadata(format!("{}: {e}", path.display())))
}

/// The workspace root cargo would pick for the package in `package_dir`.
///
/// Mirrors cargo's rule: an explicit `package.workspace` wins; a package with
/// its own `[workspace]` is its own root; otherwise the nearest ancestor with a
/// `[workspace]` that does not exclude it. A root that excludes the package is
/// skipped and the walk goes on, unless the package is also listed literally in
/// its `members`. With no root found, the package is its own.
pub(crate) fn find_workspace_root(package_dir: &Path) -> Result<PathBuf> {
    let package_dir = normalize(package_dir);
    let manifest = read_manifest(&package_dir.join("Cargo.toml"))?;
    if let Some(explicit) = manifest
        .get("package")
        .and_then(|p| p.get("workspace"))
        .and_then(|w| w.as_str())
    {
        return Ok(normalize(&package_dir.join(explicit)));
    }
    if manifest.contains_key("workspace") {
        return Ok(package_dir);
    }
    for ancestor in package_dir.ancestors().skip(1) {
        let path = ancestor.join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let candidate = read_manifest(&path)?;
        let Some(workspace) = candidate.get("workspace").and_then(|w| w.as_table()) else {
            continue;
        };
        let relative = package_dir
            .strip_prefix(ancestor)
            .expect("an ancestor is a prefix");
        if is_excluded(workspace, relative) {
            continue;
        }
        return Ok(ancestor.to_path_buf());
    }
    Ok(package_dir)
}

/// cargo's `WorkspaceRootConfig::is_excluded`: excluded by a path prefix and
/// not an explicitly listed member.
fn is_excluded(workspace: &toml::Table, relative: &Path) -> bool {
    let listed = |key: &str| -> Vec<PathBuf> {
        workspace
            .get(key)
            .and_then(|v| v.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.as_str())
                    .map(|e| normalize(Path::new(e)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let excluded = listed("exclude").iter().any(|ex| relative.starts_with(ex));
    let explicit_member = listed("members").iter().any(|m| relative.starts_with(m));
    excluded && !explicit_member
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn package(dir: &Path, extra: &str) {
        write(
            &dir.join("Cargo.toml"),
            &format!(
                "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}",
                dir.file_name().unwrap().to_str().unwrap()
            ),
        );
        write(&dir.join("src/lib.rs"), "");
    }

    #[test]
    fn workspace_roots_follow_cargo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/guests\"]\n",
        );
        package(&root.join("crates/core"), "");
        // Excluded and without a workspace of its own: its own root.
        package(&root.join("crates/guests/lonely"), "");
        // Excluded but with its own [workspace]: its own root.
        package(&root.join("crates/guests/own"), "[workspace]\n");
        // Explicit `package.workspace`.
        package(
            &root.join("crates/guests/pointed"),
            "workspace = \"../../..\"\n",
        );
        // Outside any workspace.
        package(&tmp.path().join("loose"), "");

        assert_eq!(find_workspace_root(&root.join("crates/core")).unwrap(), root);
        assert_eq!(
            find_workspace_root(&root.join("crates/guests/lonely")).unwrap(),
            root.join("crates/guests/lonely")
        );
        assert_eq!(
            find_workspace_root(&root.join("crates/guests/own")).unwrap(),
            root.join("crates/guests/own")
        );
        assert_eq!(
            find_workspace_root(&root.join("crates/guests/pointed")).unwrap(),
            root
        );
        assert_eq!(
            find_workspace_root(&tmp.path().join("loose")).unwrap(),
            tmp.path().join("loose")
        );
    }

    #[test]
    fn an_explicit_member_beats_an_exclude() {
        let mut workspace = toml::Table::new();
        workspace.insert("exclude".into(), toml::Value::Array(vec!["raster".into()]));
        workspace.insert(
            "members".into(),
            toml::Value::Array(vec!["raster/guests/g".into()]),
        );
        assert!(!is_excluded(&workspace, Path::new("raster/guests/g")));
        assert!(is_excluded(&workspace, Path::new("raster/crates/core")));
        assert!(!is_excluded(&workspace, Path::new("elsewhere")));
    }

    /// The real graph of this repository's hello-tiles example.
    #[test]
    fn resolves_hello_tiles() {
        let repo = normalize(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let packages =
            PathPackages::resolve(&repo.join("examples/hello-tiles/Cargo.toml")).unwrap();

        assert_eq!(packages.root.name, "hello-tiles");
        assert_eq!(packages.root.dir, repo.join("examples/hello-tiles"));
        for name in ["raster", "raster-core", "raster-macros"] {
            let package = packages.require(name).unwrap();
            assert_eq!(package.dir, repo.join("crates").join(name));
            assert_eq!(package.workspace_root, repo, "{name}");
        }
        assert_eq!(packages.workspace_roots(), vec![repo.clone()]);
        assert_eq!(packages.lockfile, Some(repo.join("Cargo.lock")));
        assert!(packages.packages.iter().all(|p| p.dir.is_absolute()));
    }

    #[test]
    fn a_missing_raster_names_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("no-raster");
        package(&dir, "[workspace]\n");
        let packages = PathPackages::resolve(&dir.join("Cargo.toml")).unwrap();
        let error = packages.require("raster").unwrap_err().to_string();
        assert!(error.contains("no `raster` path package"), "{error}");
        assert!(error.contains("`no-raster`"), "{error}");
    }
}
