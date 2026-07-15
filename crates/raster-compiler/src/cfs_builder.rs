//! Control Flow Schema (CFS) builder.
//!
//! This module orchestrates the generation of a CFS from a Raster project
//! by combining tile discovery, sequence discovery, and data flow resolution.

use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, ControlFlowSchema, EntrypointItem, InputBinding,
    SequenceChildId, SequenceChildItem, SequenceDef, SequenceId, SequenceItem, TileDef,
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
            let seq_def = self.build_sequence_def(seq)?;
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
    ///
    /// `main`'s declared parameters are entry arguments (see
    /// `SequenceChildItem::Entrypoint`), not caller-supplied `SequenceScope`
    /// parameters — main has no caller. When `main` declares any, it gets a
    /// leading `Entrypoint` item instead of the usual `external()`
    /// `input_sources`, and every other item's `PriorItemOutput` addressing
    /// shifts by one to make room for it. Every other sequence (including
    /// `main` with no declared parameters) is built exactly as before.
    fn build_sequence_def(&self, seq: &Sequence<'_>) -> Result<SequenceDef> {
        let mut resolver = FlowResolver::new();

        if seq.function.name == "main" && !seq.function.input_names.is_empty() {
            let entry_item = SequenceChildItem::Entrypoint(EntrypointItem {
                names: seq.function.input_names.clone(),
            });
            let mut items = vec![entry_item];
            items.extend(resolver.resolve_with_entry_arguments(seq, &seq.function.input_names));

            return Ok(SequenceDef {
                id: seq.function.name.clone(),
                input_sources: Vec::new(),
                items,
            });
        }

        // A sequence definition's own parameters are supplied by whoever
        // calls it: parameter `i` is sequence-scope slot `i`. (Callers'
        // arguments are resolved separately, at each call site.)
        let input_count = seq.function.inputs.len();
        let input_sources: Vec<InputBinding> =
            (0..input_count).map(InputBinding::seq_input).collect();

        // Resolve data flow for the sequence items
        let items = resolver.resolve(seq);

        Ok(SequenceDef {
            id: seq.function.name.clone(),
            input_sources,
            items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        CallArgumentKind, CallInfo, CallKind, FunctionAstItem, MacroAstItem, ProjectAst,
    };
    use crate::sequence::SequenceStep;
    use crate::tile::Tile;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn mock_project() -> Project {
        Project {
            name: "test".to_string(),
            ast: ProjectAst {
                name: "test".to_string(),
                root_path: PathBuf::from("/test"),
                functions: vec![],
            },
            root_dir: PathBuf::from("/test"),
            output_dir: PathBuf::from("/test/target/raster"),
            target_dir: PathBuf::from("/test/target/"),
        }
    }

    fn tile_function(name: &str) -> FunctionAstItem {
        FunctionAstItem {
            name: name.to_string(),
            path: PathBuf::from("test.rs"),
            call_infos: vec![],
            macros: vec![MacroAstItem {
                name: "tile".to_string(),
                args: HashMap::new(),
            }],
            input_names: vec!["input".to_string()],
            inputs: vec!["String".to_string()],
            output: Some("String".to_string()),
            signature: format!("fn {}()", name),
            selection_aliases: vec![],
        }
    }

    fn main_function_with_params(
        input_names: Vec<&str>,
        call_infos: Vec<CallInfo>,
    ) -> FunctionAstItem {
        FunctionAstItem {
            name: "main".to_string(),
            path: PathBuf::from("test.rs"),
            call_infos,
            macros: vec![MacroAstItem {
                name: "sequence".to_string(),
                args: HashMap::new(),
            }],
            input_names: input_names.iter().map(|s| s.to_string()).collect(),
            inputs: input_names
                .iter()
                .map(|_| "PersonalData".to_string())
                .collect(),
            output: None,
            signature: "fn main()".to_string(),
            selection_aliases: vec![],
        }
    }

    #[test]
    fn main_with_entry_arguments_gets_a_leading_entrypoint_item_and_empty_input_sources() {
        let project = mock_project();
        let greet_func = tile_function("greet");
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        let main_func = main_function_with_params(
            vec!["personal_data", "seed"],
            vec![CallInfo {
                callee: "greet".to_string(),
                result_binding: Some("greeting".to_string()),
                arguments: vec!["personal_data".to_string()],
                argument_kinds: vec![CallArgumentKind::Rooted {
                    root: "personal_data".to_string(),
                }],
                call_kind: CallKind::Tile,
            }],
        );
        let sequence = Sequence {
            function: &main_func,
            steps: vec![SequenceStep::Tile(&greet_tile)],
            description: None,
        };

        let builder = CfsBuilder::new(&project);
        let seq_def = builder.build_sequence_def(&sequence).unwrap();

        assert!(
            seq_def.input_sources.is_empty(),
            "main's own input_sources must be empty once its parameters become an Entrypoint item"
        );
        assert_eq!(seq_def.items.len(), 2, "Entrypoint item + one tile item");

        match &seq_def.items[0] {
            SequenceChildItem::Entrypoint(item) => {
                assert_eq!(
                    item.names,
                    vec!["personal_data".to_string(), "seed".to_string()]
                );
            }
            other => panic!("Expected a leading Entrypoint item, got {:?}", other),
        }

        match &seq_def.items[1] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "greet");
                match &tile_item.sources[0] {
                    InputBinding::PriorItemOutput {
                        intra_sequence_item_index,
                    } => assert_eq!(*intra_sequence_item_index, 0),
                    other => panic!("Expected PriorItemOutput{{0}}, got {:?}", other),
                }
            }
            other => panic!("Expected a Tile item, got {:?}", other),
        }
    }

    #[test]
    fn main_without_parameters_is_unaffected() {
        let project = mock_project();
        let greet_func = tile_function("greet");
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        let main_func = main_function_with_params(
            vec![],
            vec![CallInfo {
                callee: "greet".to_string(),
                result_binding: None,
                arguments: vec!["\"Raster\".to_string()".to_string()],
                argument_kinds: vec![CallArgumentKind::Inline],
                call_kind: CallKind::Tile,
            }],
        );
        let sequence = Sequence {
            function: &main_func,
            steps: vec![SequenceStep::Tile(&greet_tile)],
            description: None,
        };

        let builder = CfsBuilder::new(&project);
        let seq_def = builder.build_sequence_def(&sequence).unwrap();

        assert!(
            seq_def.input_sources.is_empty(),
            "main declared zero parameters"
        );
        assert_eq!(seq_def.items.len(), 1, "no Entrypoint item should be added");
        assert!(matches!(seq_def.items[0], SequenceChildItem::Tile(_)));
    }
}
