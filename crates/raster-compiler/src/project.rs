use std::path::Path;
use std::path::PathBuf;

use crate::ast::ProjectAst;
use anyhow::{anyhow, Result};

use crate::tile::TileDiscovery;
use crate::tile::Tile;

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub ast: ProjectAst,

    /// Path to the project root.
    /// TODO: Change to &Path
    pub root_path: PathBuf,
}

impl Project {
    pub fn new(project_path: PathBuf) -> Result<Self> {
        let name = Self::project_name(&project_path)?;
        let ast = ProjectAst::new(&project_path)?;

        Ok(Self {
            name,
            ast,
            root_path: project_path,
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

        Err(anyhow!("Could not determine project name"))
    }

}

impl Project {
    pub fn tiles(&self) -> Vec<Tile<'_>> {
        // TODO: Lazy load tiles from the project
        TileDiscovery::new(self).tiles
    }
}
