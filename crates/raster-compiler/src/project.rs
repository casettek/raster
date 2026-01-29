use std::path::Path;
use std::path::PathBuf;

use raster_core::{Error, Result};
use crate::ast::ProjectAst;

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub ast: ProjectAst,

    /// Path to the project root.
    /// TODO: Change to &Path
    pub root_dir: PathBuf,
    pub output_dir: PathBuf,
}

impl Project {
    pub fn new(root_dir: PathBuf) -> Result<Self> {
        let name = Self::project_name(&root_dir)?;
        let ast = ProjectAst::new(&root_dir)?;

        let output_dir = root_dir.join("target").join("raster");

        Ok(Self {
            name,
            ast,
            root_dir,
            output_dir,
        })
    }

    fn project_name(project_root: &Path) -> Result<String> {
        let cargo_toml_path = project_root.join("Cargo.toml");
        let content = std::fs::read_to_string(&cargo_toml_path)?;

        // Simple parsing - look for name = "..."
        for line in content.lines().map(|line| line.trim()) {
            if line.starts_with("name") {
                if let Some(eq_pos) = line.find('=') {
                    let value = line[eq_pos + 1..].trim();
                    let value = value.trim_matches('"').trim_matches('\'');
                    if !value.is_empty() {
                        return Ok(value.to_string());
                    }
                }
            }
        }

        Err(Error::Other("Could not determine project name".into()))
    }

}

pub struct ProjectDiscovery {
    pub project: Project,
    pub project_ast: ProjectAst,
}

impl ProjectDiscovery {
    pub fn new(project: Project) -> Result<Self> {
        let project_ast = ProjectAst::new(&project.root_dir)?;

        Ok(Self { project, project_ast })
    }
}