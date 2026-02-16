use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::ast::ProjectAst;
use cargo_toml::Manifest;
use raster_core::{Error, Result};

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub ast: ProjectAst,

    /// Path to the project root.
    /// TODO: Change to &Path
    pub root_dir: PathBuf,
    pub output_dir: PathBuf,
    pub target_dir: PathBuf,
}

impl Project {
    pub fn new(root_dir: PathBuf) -> Result<Self> {
        let name = Self::project_name(&root_dir);
        let target_dir =
            Self::find_target_path(&root_dir).unwrap_or_else(|| root_dir.join("target"));

        let ast = ProjectAst::new(&root_dir)?;

        let output_dir = root_dir.join("target").join("raster");

        Ok(Self {
            name,
            ast,
            root_dir,
            output_dir,
            target_dir,
        })
    }

    fn project_name(project_root: &Path) -> String {
        let manifest =
            Manifest::from_path(project_root.join("Cargo.toml")).expect("No manifest found");

        manifest
            .package
            .map(|p| p.name)
            .expect("No [package] found")
    }

    fn find_target_path(project_path: &std::path::Path) -> Option<PathBuf> {
        // Run cargo metadata to get the target directory
        let output = Command::new("cargo")
            .current_dir(project_path)
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8(output.stdout).ok()?;
        let meta: serde_json::Value = serde_json::from_str(&stdout).ok()?;

        meta.get("target_directory")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    }
}

pub struct ProjectDiscovery {
    pub project: Project,
    pub project_ast: ProjectAst,
}

impl ProjectDiscovery {
    pub fn new(project: Project) -> Result<Self> {
        let project_ast = ProjectAst::new(&project.root_dir)?;

        Ok(Self {
            project,
            project_ast,
        })
    }
}
