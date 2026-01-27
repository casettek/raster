//! Control Flow Schema (CFS) builder.
//!
//! This module orchestrates the generation of a CFS from a Raster project
//! by combining tile discovery, sequence discovery, and data flow resolution.

use raster_core::cfs::{ControlFlowSchema, InputBinding, SequenceDef, TileDef};

use raster_core::Result;
use crate::Project;
use crate::flow_resolver::FlowResolver;
use crate::sequence::{Sequence, SequenceDiscovery};
use crate::tile::TileDiscovery;

/// Builds a control flow schema from a Raster project.
pub struct CfsBuilder<'a> {
    project: &'a Project,
}

impl<'a> CfsBuilder<'a> {
    /// Create a new CFS builder with the given project name.
    pub fn new(project: &'a Project) -> Self {
        Self {
            project,
        }
    }

    /// Build the CFS from a project root directory.
    pub fn build(&self) -> Result<ControlFlowSchema> {
        // Parse the project AST

        // Discover tiles from AST
        let tile_discovery = TileDiscovery::new(self.project);

        // Discover sequences from AST
        let sequence_discovery = SequenceDiscovery::new(self.project, &tile_discovery);

        // Build tile definitions
        let tiles: Vec<TileDef> = tile_discovery
            .tiles
            .iter()
            .map(|t| {
                let input_count = t.function.inputs.len();
                let output_count = if t.function.output.is_some() { 1 } else { 0 };
                TileDef::new(&t.function.name, input_count, output_count)
            })
            .collect();

        // Build sequence definitions with resolved data flow
        let mut sequences = Vec::new();
        for seq in &sequence_discovery.sequences {
            let seq_def = self.build_sequence_def(seq, &tile_discovery, &sequence_discovery)?;
            sequences.push(seq_def);
        }

        Ok(ControlFlowSchema {
            version: "1.0".to_string(),
            project: self.project.name.clone(),
            encoding: "postcard".to_string(),
            tiles,
            sequences,
        })
    }

    /// Build a sequence definition from a discovered sequence.
    fn build_sequence_def<'ast>(
        &self,
        seq: &Sequence<'ast>,
        tile_discovery: &TileDiscovery<'ast>,
        sequence_discovery: &SequenceDiscovery<'ast>,
    ) -> Result<SequenceDef> {
        // Create input sources for the sequence's parameters
        // All sequence inputs come from external sources
        let input_count = seq.function.inputs.len();
        let input_sources: Vec<InputBinding> = (0..input_count)
            .map(|_| InputBinding::external())
            .collect();

        // Resolve data flow for the sequence items
        let mut resolver = FlowResolver::new(tile_discovery, sequence_discovery);
        let items = resolver.resolve(seq);

        Ok(SequenceDef {
            id: seq.function.name.clone(),
            input_sources,
            items,
        })
    }
}
