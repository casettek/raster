//! Control Flow Schema (CFS) builder.
//!
//! This module orchestrates the generation of a CFS from a Raster project
//! by combining tile discovery, sequence discovery, and data flow resolution.

use raster_core::cfs::{
    CfsCoordinates, ControlFlowSchema, InputBinding, SequenceChild, SequenceDef, SequenceId,
    TileDef,
};

use crate::flow_resolver::FlowResolver;
use crate::sequence::{Sequence, SequenceDiscovery};
use crate::tile::TileDiscovery;
use crate::Project;
use raster_core::Result;

/// Builds a control flow schema from a Raster project.
pub struct CfsBuilder<'a> {
    project: &'a Project,
}

impl<'a> CfsBuilder<'a> {
    /// Create a new CFS builder with the given project name.
    pub fn new(project: &'a Project) -> Self {
        Self { project }
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
                TileDef::new(&t.function.name, &t.tile_type, input_count, output_count)
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
        let input_sources: Vec<InputBinding> =
            (0..input_count).map(|_| InputBinding::external()).collect();

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

#[derive(Clone)]
pub struct CfsResolver {
    pub cfs: ControlFlowSchema,
}

impl CfsResolver {
    pub fn new(cfs: ControlFlowSchema) -> Self {
        CfsResolver { cfs }
    }

    pub fn resolve(&self, coords: &CfsCoordinates) -> &SequenceDef {
        let (start_coord, rest_coords) = coords
            .0
            .split_first()
            .map(|(h, t)| (*h as usize, t))
            .unwrap_or_else(|| {
                let main_pos = self
                    .cfs
                    .sequences
                    .iter()
                    .position(|s| s.id == "main")
                    .expect("Missing main entrypoint");
                (main_pos, &[][..])
            });

        let mut current_sequence = self
            .cfs
            .sequences
            .get(start_coord)
            .expect("Wrong cfs coordinates");

        for &coord in rest_coords {
            let child_sequence_id = current_sequence
                .items
                .iter()
                .filter_map(|item| match item {
                    SequenceChild::Tile(_) => None,
                    SequenceChild::Sequence(sequence) => Some(sequence.id.clone()),
                })
                .nth(coord as usize)
                .expect("Could not resolve sequence coordinates");

            current_sequence = self
                .cfs
                .sequences
                .iter()
                .find(|sequence| *sequence.id == *child_sequence_id)
                .expect("Wrong cfs coordinates");
        }

        current_sequence
    }

    pub fn get_pos(&self, sequence_id: SequenceId) -> Option<usize> {
        self.cfs
            .sequences
            .iter()
            .position(|sequence| sequence.id == sequence_id)
    }

    pub fn get_coordinates(
        &self,
        parent_coords: &CfsCoordinates,
        sequence_id: SequenceId,
    ) -> CfsCoordinates {
        if parent_coords.is_empty() {
            let main_entrypoint_pos = self
                .get_pos(String::from("main"))
                .expect("Missing main in cfs");

            let mut current_coords = parent_coords.clone();
            current_coords.push(
                main_entrypoint_pos
                    .try_into()
                    .expect("entrypoint coordinates not fit to u8"),
            );

            return current_coords;
        }
        println!("[debug]CfsResolver::get_coordinates");

        println!("[debug]parent_coords: {:?}", parent_coords);
        println!("[debug]sequence_id: {:?}", sequence_id);

        let parent_sequence = self.resolve(parent_coords);
        println!(
            "[debug]parent_sequence.items: {:?}",
            parent_sequence
                .sequences()
                .iter()
                .map(|seq| seq.id.clone())
                .collect::<Vec<_>>()
        );

        let sequence_coord = parent_sequence
            .sequences()
            .iter()
            .position(|item| item.id == sequence_id)
            .unwrap_or_else(|| {
                panic!(
                    "Wrong coordinates for sequence '{}': [{} [{:?}] {:?}]",
                    sequence_id,
                    parent_sequence.id,
                    parent_coords,
                    parent_sequence
                        .sequences()
                        .iter()
                        .map(|seq| seq.id.clone())
                        .collect::<Vec<_>>()
                )
            });

        println!("[debug]sequence_coord: {:?}", sequence_coord);

        let mut current_coords = parent_coords.clone();
        current_coords.push(
            sequence_coord
                .try_into()
                .expect("Sequence coordinate out ouf bound u8"),
        );

        println!("[debug]current_coords: {:?}", current_coords);
        println!("[debug]                                          ");
        println!("[debug]                                          ");
        println!("[debug]                                          ");
        current_coords
    }
}
