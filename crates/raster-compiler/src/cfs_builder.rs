//! Control Flow Schema (CFS) builder.
//!
//! This module orchestrates the generation of a CFS from a Raster project
//! by combining tile discovery, sequence discovery, and data flow resolution.

use raster_core::cfs::{ControlFlowSchema, InputBinding, SequenceDef, TileDef};
use raster_core::{Error, Result};
use std::path::Path;

use crate::discovery::{SequenceDiscovery, TileDiscovery};
use crate::flow_resolver::FlowResolver;

/// Builds a control flow schema from a Raster project.
pub struct CfsBuilder {
    /// Project name (from Cargo.toml or provided).
    project_name: String,
}

impl CfsBuilder {
    /// Create a new CFS builder with the given project name.
    pub fn new(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
        }
    }

    /// Build the CFS from a project root directory.
    pub fn build(&self, project_root: &Path) -> Result<ControlFlowSchema> {
        // Discover tiles
        let tile_discovery = TileDiscovery::new(project_root);
        let discovered_tiles = tile_discovery.discover()?;

        // Discover sequences
        let sequence_discovery = SequenceDiscovery::new(project_root);
        let discovered_sequences = sequence_discovery.discover()?;

        // Build tile definitions
        let tiles: Vec<TileDef> = discovered_tiles
            .iter()
            .map(|t| TileDef::new(&t.metadata.id.0, t.input_count, t.output_count))
            .collect();

        // Build sequence definitions with resolved data flow
        let mut sequences = Vec::new();
        for seq in &discovered_sequences {
            let seq_def = self.build_sequence_def(seq, &discovered_tiles, &discovered_sequences)?;
            sequences.push(seq_def);
        }

        Ok(ControlFlowSchema {
            version: "1.0".to_string(),
            project: self.project_name.clone(),
            encoding: "postcard".to_string(),
            tiles,
            sequences,
        })
    }

    /// Build a sequence definition from a discovered sequence.
    fn build_sequence_def(
        &self,
        seq: &crate::discovery::DiscoveredSequence,
        tiles: &[crate::discovery::DiscoveredTile],
        sequences: &[crate::discovery::DiscoveredSequence],
    ) -> Result<SequenceDef> {
        // Create input sources for the sequence's parameters
        // All sequence inputs come from external sources
        let input_sources: Vec<InputBinding> = (0..seq.input_count)
            .map(|_| InputBinding::external())
            .collect();

        // Resolve data flow for the sequence items
        let mut resolver = FlowResolver::new(tiles, sequences);
        let items = resolver.resolve(seq);

        Ok(SequenceDef {
            id: seq.id.clone(),
            input_sources,
            items,
        })
    }
}

/// Try to extract project name from Cargo.toml.
pub fn extract_project_name(project_root: &Path) -> Result<String> {
    let cargo_toml_path = project_root.join("Cargo.toml");
    let content = std::fs::read_to_string(&cargo_toml_path).map_err(Error::Io)?;

    // Simple parsing - look for name = "..."
    for line in content.lines() {
        let line = line.trim();
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

    // Fallback to directory name
    project_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Other("Could not determine project name".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cfs_builder_new() {
        let builder = CfsBuilder::new("test-project");
        assert_eq!(builder.project_name, "test-project");
    }
}
