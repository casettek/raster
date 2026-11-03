//! The stage: a temp directory a guest is built from, laid out so cargo sees
//! every path package by a relative path.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

/// The stage name of the entry that holds the `raster` workspace. Fixed, so
/// the raster checkout's directory name never reaches an image id.
pub const RASTER_ENTRY: &str = "raster";

/// One top-level stage entry: `stage/<name>` links to `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEntry {
    pub name: String,
    pub target: PathBuf,
}

/// Where each path package appears inside a stage.
///
/// Let A be the common ancestor of the workspace roots, moved up one level if
/// it is itself one of them. Each top-level entry under A that holds a
/// workspace root is staged under its own name, except the one holding the
/// `raster` workspace, which is staged as [`RASTER_ENTRY`]. A sits strictly
/// above every workspace root so the stage root's own manifest never replaces
/// one: raster's root manifest carries the `[workspace.dependencies]` its
/// crates inherit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    ancestor: PathBuf,
    entries: Vec<StageEntry>,
}

impl StagePlan {
    /// Plan a stage for path packages in `workspace_roots`. `raster_root` is
    /// the workspace root holding the `raster` package, if the guest uses it.
    pub fn new(workspace_roots: &[PathBuf], raster_root: Option<&Path>) -> Result<Self> {
        let mut roots: Vec<PathBuf> = workspace_roots.iter().map(|r| normalize(r)).collect();
        roots.sort();
        roots.dedup();
        if roots.is_empty() {
            return Err(Error::Layout("no path packages to stage".into()));
        }
        if let Some(relative) = roots.iter().find(|r| !r.is_absolute()) {
            return Err(Error::Layout(format!(
                "workspace root {} is not absolute",
                relative.display()
            )));
        }

        let mut ancestor = common_ancestor(&roots).ok_or_else(|| {
            Error::Layout("the path packages share no common directory".into())
        })?;
        if roots.contains(&ancestor) {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| {
                    Error::Layout("a workspace root is the filesystem root".into())
                })?
                .to_path_buf();
        }

        let raster_entry = raster_root.map(|r| top_entry(&ancestor, &normalize(r)));
        let mut by_name: BTreeMap<String, PathBuf> = BTreeMap::new();
        for root in &roots {
            let entry = top_entry(&ancestor, root);
            let name = if raster_entry.as_ref() == Some(&entry) {
                RASTER_ENTRY.to_string()
            } else {
                entry.to_str().map(str::to_string).ok_or_else(|| {
                    Error::Layout(format!("{} is not valid UTF-8", entry.to_string_lossy()))
                })?
            };
            let target = ancestor.join(&entry);
            if let Some(previous) = by_name.insert(name.clone(), target.clone()) {
                if previous != target {
                    return Err(Error::Layout(format!(
                        "both {} and {} would be staged as `{name}`",
                        previous.display(),
                        target.display()
                    )));
                }
            }
        }

        Ok(Self {
            ancestor,
            entries: by_name
                .into_iter()
                .map(|(name, target)| StageEntry { name, target })
                .collect(),
        })
    }

    /// The directory the stage mirrors (A).
    pub fn ancestor(&self) -> &Path {
        &self.ancestor
    }

    /// The staged entries, sorted by name.
    pub fn entries(&self) -> &[StageEntry] {
        &self.entries
    }

    /// Where the real directory `real` appears in the stage, relative to the
    /// stage root.
    pub fn stage_relative(&self, real: &Path) -> Result<PathBuf> {
        let real = normalize(real);
        let outside = || {
            Error::Layout(format!(
                "{} is outside every staged entry under {}",
                real.display(),
                self.ancestor.display()
            ))
        };
        let relative = real.strip_prefix(&self.ancestor).map_err(|_| outside())?;
        let mut components = relative.components();
        let first = components.next().ok_or_else(outside)?;
        let target = self.ancestor.join(first);
        let entry = self
            .entries
            .iter()
            .find(|e| e.target == target)
            .ok_or_else(outside)?;
        Ok(Path::new(&entry.name).join(components.as_path()))
    }
}

/// A created stage. The directory is removed on drop.
pub struct Stage {
    dir: tempfile::TempDir,
    plan: StagePlan,
}

impl Stage {
    /// Create the stage in the system temp directory.
    pub fn create(plan: StagePlan) -> Result<Self> {
        Self::create_in(plan, None)
    }

    /// Create the stage under `parent`, or the system temp directory.
    pub fn create_in(plan: StagePlan, parent: Option<&Path>) -> Result<Self> {
        let builder = {
            let mut b = tempfile::Builder::new();
            b.prefix("raster-stage-");
            b
        };
        let dir = match parent {
            Some(parent) => builder.tempdir_in(parent)?,
            None => builder.tempdir()?,
        };
        for entry in &plan.entries {
            link(&entry.target, &dir.path().join(&entry.name))?;
        }
        Ok(Self { dir, plan })
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn plan(&self) -> &StagePlan {
        &self.plan
    }

    /// Where the real directory `real` appears in this stage, relative to its
    /// root.
    pub fn relative(&self, real: &Path) -> Result<PathBuf> {
        self.plan.stage_relative(real)
    }
}

#[cfg(unix)]
pub(crate) fn link(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at)?;
    Ok(())
}

/// Without unix symlinks, copy the tree, minus build output and VCS metadata.
#[cfg(not(unix))]
pub(crate) fn link(target: &Path, at: &Path) -> Result<()> {
    fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "target" || name == ".git" {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                copy(&entry.path(), &to.join(&name))?;
            } else {
                std::fs::copy(entry.path(), to.join(&name))?;
            }
        }
        Ok(())
    }
    copy(target, at)?;
    Ok(())
}

/// `path` with `.` dropped and `..` applied, without touching the filesystem.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut common: Vec<Component> = paths.first()?.components().collect();
    for path in &paths[1..] {
        let length = common
            .iter()
            .zip(path.components())
            .take_while(|(a, b)| *a == b)
            .count();
        common.truncate(length);
    }
    (!common.is_empty()).then(|| common.iter().collect())
}

/// The first component of `root` below `ancestor`.
fn top_entry(ancestor: &Path, root: &Path) -> OsString {
    root.strip_prefix(ancestor)
        .ok()
        .and_then(|r| r.components().next())
        .map(|c| c.as_os_str().to_os_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn names(plan: &StagePlan) -> Vec<(&str, &Path)> {
        plan.entries()
            .iter()
            .map(|e| (e.name.as_str(), e.target.as_path()))
            .collect()
    }

    /// An in-repo example: one workspace, the raster checkout.
    #[test]
    fn a_single_workspace_is_staged_one_level_up() {
        let checkout = p("/home/u/dev/raster");
        let plan = StagePlan::new(std::slice::from_ref(&checkout), Some(&checkout)).unwrap();
        assert_eq!(plan.ancestor(), Path::new("/home/u/dev"));
        assert_eq!(names(&plan), vec![("raster", checkout.as_path())]);
        assert_eq!(
            plan.stage_relative(&checkout.join("crates/raster-core")).unwrap(),
            p("raster/crates/raster-core")
        );
    }

    /// The raster checkout's own directory name does not reach the stage.
    #[test]
    fn the_raster_checkout_is_always_staged_as_raster() {
        let checkout = p("/tmp/x/defender-raster");
        let plan = StagePlan::new(std::slice::from_ref(&checkout), Some(&checkout)).unwrap();
        assert_eq!(names(&plan), vec![("raster", checkout.as_path())]);
        assert_eq!(
            plan.stage_relative(&checkout.join("examples/hello-tiles")).unwrap(),
            p("raster/examples/hello-tiles")
        );
    }

    /// iris next to raster.
    #[test]
    fn a_sibling_project_keeps_its_name() {
        let raster = p("/home/u/dev/raster-project/raster");
        let iris = p("/home/u/dev/raster-project/iris");
        let plan = StagePlan::new(&[iris.clone(), raster.clone()], Some(&raster)).unwrap();
        assert_eq!(plan.ancestor(), Path::new("/home/u/dev/raster-project"));
        assert_eq!(
            names(&plan),
            vec![("iris", iris.as_path()), ("raster", raster.as_path())]
        );
        assert_eq!(
            plan.stage_relative(&iris.join("raster-stages/prefill-range")).unwrap(),
            p("iris/raster-stages/prefill-range")
        );
    }

    /// A protocol guest that still has its own `[workspace]` inside the
    /// checkout: one entry, the checkout.
    #[test]
    fn a_nested_workspace_stays_inside_its_entry() {
        let checkout = p("/w/raster");
        let guest = checkout.join("crates/raster-prover/guests/transition");
        let plan = StagePlan::new(&[checkout.clone(), guest.clone()], Some(&checkout)).unwrap();
        assert_eq!(plan.ancestor(), Path::new("/w"));
        assert_eq!(names(&plan), vec![("raster", checkout.as_path())]);
        assert_eq!(
            plan.stage_relative(&guest).unwrap(),
            p("raster/crates/raster-prover/guests/transition")
        );
    }

    #[test]
    fn a_layout_without_raster_keeps_real_names() {
        let plan = StagePlan::new(&[p("/a/b/pkg")], None).unwrap();
        assert_eq!(names(&plan), vec![("pkg", Path::new("/a/b/pkg"))]);
    }

    #[test]
    fn a_name_collision_with_raster_is_an_error() {
        let raster = p("/p/raster-main");
        let other = p("/p/raster");
        let error = StagePlan::new(&[raster.clone(), other], Some(&raster))
            .unwrap_err()
            .to_string();
        assert!(error.contains("staged as `raster`"), "{error}");
    }

    #[test]
    fn paths_outside_the_stage_are_errors() {
        let plan = StagePlan::new(&[p("/p/raster")], Some(Path::new("/p/raster"))).unwrap();
        assert!(plan.stage_relative(Path::new("/p/elsewhere")).is_err());
        assert!(plan.stage_relative(Path::new("/q/raster")).is_err());
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            p("/a/c/d")
        );
        assert_eq!(
            normalize(Path::new("/p/iris/raster-stages/x/../../../raster/crates/raster")),
            p("/p/raster/crates/raster")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_created_stage_links_its_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("some-checkout");
        std::fs::create_dir_all(checkout.join("crates")).unwrap();
        let plan = StagePlan::new(std::slice::from_ref(&checkout), Some(&checkout)).unwrap();
        let stage = Stage::create_in(plan, Some(tmp.path())).unwrap();
        let link = stage.root().join("raster");
        assert_eq!(std::fs::read_link(&link).unwrap(), checkout);
        assert!(link.join("crates").is_dir());
        let root = stage.root().to_path_buf();
        drop(stage);
        assert!(!root.exists(), "the stage is removed on drop");
        assert!(checkout.join("crates").is_dir(), "the checkout is untouched");
    }
}
