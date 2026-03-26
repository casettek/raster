//! Data flow resolution for sequences.
//!
//! This module resolves how data flows between tiles in a sequence by tracking
//! variable bindings and mapping them to `InputSource` references.

use raster_core::cfs::{InputBinding, InputSource, SequenceChildItem, SequenceItem, TileItem};
use std::collections::HashMap;

use crate::ast::{CallInfo, CallKind};
use crate::sequence::{Sequence, SequenceDiscovery};
use crate::tile::TileDiscovery;

/// Resolves data flow within a sequence, producing `SequenceItem`s with
/// correctly bound input sources.
pub struct FlowResolver<'a, 'ast> {
    /// Map of variable names to their source (item_index, output_index).
    bindings: HashMap<String, (usize, usize)>,
    /// Sequence parameter names mapped to their input index.
    param_indices: HashMap<String, usize>,
    /// Known tiles for looking up output counts.
    tile_discovery: &'a TileDiscovery<'ast>,
    /// Known sequences for looking up output counts (for nested sequences).
    sequence_discovery: &'a SequenceDiscovery<'ast>,
}

impl<'a, 'ast> FlowResolver<'a, 'ast> {
    /// Create a new flow resolver.
    pub fn new(
        tile_discovery: &'a TileDiscovery<'ast>,
        sequence_discovery: &'a SequenceDiscovery<'ast>,
    ) -> Self {
        Self {
            bindings: HashMap::new(),
            param_indices: HashMap::new(),
            tile_discovery,
            sequence_discovery,
        }
    }

    /// Resolve a discovered sequence into a list of `SequenceChild`s with input sources.
    pub fn resolve(&mut self, sequence: &Sequence<'ast>) -> Vec<SequenceChildItem> {
        // Reset state for this sequence
        self.bindings.clear();
        self.param_indices.clear();

        // Map sequence parameters to their indices
        for (idx, name) in sequence.function.input_names.iter().enumerate() {
            self.param_indices.insert(name.clone(), idx);
        }

        let mut items = Vec::new();

        // Collect call_infos that correspond to validated sequence steps.
        // Unknown callees are already rejected and diagnosed by SequenceDiscovery::extract_sequence
        // (in sequence.rs) — only validated calls survive into sequence.steps and reach the resolver.
        // We filter by step membership here to stay in sync with what discovery accepted.
        let step_callees: Vec<&str> = sequence
            .steps
            .iter()
            .map(|step| match step {
                crate::sequence::SequenceStep::Tile(tile) => tile.function.name.as_str(),
                crate::sequence::SequenceStep::Sequence(name) => name.as_str(),
            })
            .collect();

        let relevant_calls: Vec<&CallInfo> = sequence
            .function
            .call_infos
            .iter()
            .filter(|call| step_callees.contains(&call.callee.as_str()))
            .collect();

        for (item_index, call) in relevant_calls.iter().enumerate() {
            let input_sources = self.resolve_call_inputs(call);

            // Call kind directly determines item type — no name-matching needed.
            let item = match call.call_kind {
                CallKind::Tile => SequenceChildItem::Tile(TileItem {
                    id: call.callee.clone(),
                    sources: input_sources,
                }),
                CallKind::Sequence => SequenceChildItem::Sequence(SequenceItem {
                    id: call.callee.clone(),
                    sources: input_sources,
                }),
            };

            items.push(item);

            // If this call has a result binding, record it
            if let Some(ref binding_name) = call.result_binding {
                // For now, assume single output (output_index = 0)
                self.bindings.insert(binding_name.clone(), (item_index, 0));
            }
        }

        items
    }

    /// Resolve input sources for a function call's arguments.
    fn resolve_call_inputs(&self, call: &CallInfo) -> Vec<InputBinding> {
        call.arguments
            .iter()
            .map(|arg| self.resolve_argument(arg))
            .collect()
    }

    /// Resolve a single argument to its input source.
    fn resolve_argument(&self, arg: &str) -> InputBinding {
        let arg = arg.trim();

        // Check if it's a sequence parameter
        if let Some(&idx) = self.param_indices.get(arg) {
            return InputBinding::seq_input(idx);
        }

        // Check if it's a bound variable from a previous item
        if let Some(&(item_index, output_index)) = self.bindings.get(arg) {
            return InputBinding::item_output(item_index, output_index);
        }

        // If we can't resolve it, treat it as coming from a sequence input
        // This handles cases like literals or complex expressions
        // For now, we'll use external as a fallback
        // In a more complete implementation, we might want to handle this differently
        InputBinding::new(InputSource::External)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionAstItem;
    use crate::ast::MacroAstItem;
    use crate::ast::ProjectAst;
    use crate::sequence::SequenceStep;
    use crate::tile::Tile;
    use crate::Project;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_mock_project() -> Project {
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

    fn make_tile_function(name: &str, input_names: Vec<&str>, has_output: bool) -> FunctionAstItem {
        FunctionAstItem {
            name: name.to_string(),
            path: PathBuf::from("test.rs"),
            call_infos: vec![],
            macros: vec![MacroAstItem {
                name: "tile".to_string(),
                args: HashMap::new(),
            }],
            input_names: input_names.iter().map(|s| s.to_string()).collect(),
            inputs: input_names.iter().map(|_| "String".to_string()).collect(),
            output: if has_output {
                Some("String".to_string())
            } else {
                None
            },
            signature: format!("fn {}()", name),
        }
    }

    fn make_sequence_function(
        name: &str,
        input_names: Vec<&str>,
        call_infos: Vec<CallInfo>,
    ) -> FunctionAstItem {
        FunctionAstItem {
            name: name.to_string(),
            path: PathBuf::from("test.rs"),
            call_infos,
            macros: vec![MacroAstItem {
                name: "sequence".to_string(),
                args: HashMap::new(),
            }],
            input_names: input_names.iter().map(|s| s.to_string()).collect(),
            inputs: input_names.iter().map(|_| "String".to_string()).collect(),
            output: Some("String".to_string()),
            signature: format!("fn {}()", name),
        }
    }

    #[test]
    fn test_resolve_simple_sequence() {
        // Create mock project for discovery structs
        let project = make_mock_project();

        // Create tile functions
        let greet_func = make_tile_function("greet", vec!["input"], true);
        let exclaim_func = make_tile_function("exclaim", vec!["input"], true);

        // Create tiles
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "tile".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let exclaim_tile = Tile {
            function: &exclaim_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![greet_tile, exclaim_tile],
        };
        let sequence_discovery = SequenceDiscovery {
            project: &project,
            sequences: vec![],
        };

        // Create sequence function with call infos
        let seq_func = make_sequence_function(
            "main",
            vec!["name"],
            vec![
                CallInfo {
                    callee: "greet".to_string(),
                    result_binding: Some("greeting".to_string()),
                    arguments: vec!["name".to_string()],
                    call_kind: CallKind::Tile,
                },
                CallInfo {
                    callee: "exclaim".to_string(),
                    result_binding: None,
                    arguments: vec!["greeting".to_string()],
                    call_kind: CallKind::Tile,
                },
            ],
        );

        let sequence = Sequence {
            function: &seq_func,
            steps: vec![
                SequenceStep::Tile(&tile_discovery.tiles[0]),
                SequenceStep::Tile(&tile_discovery.tiles[1]),
            ],
            description: None,
        };

        let mut resolver = FlowResolver::new(&tile_discovery, &sequence_discovery);
        let items = resolver.resolve(&sequence);

        assert_eq!(items.len(), 2);

        // First item: greet(name) where name is seq_input[0]
        match &items[0] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "greet");
                assert_eq!(tile_item.sources.len(), 1);
                match &tile_item.sources[0].source {
                    InputSource::SeqInput { input_index } => assert_eq!(*input_index, 0),
                    _ => panic!("Expected SeqInput"),
                }
            }
            _ => panic!("Expected Tile item"),
        }

        // Second item: exclaim(greeting) where greeting is item_output[0][0]
        match &items[1] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "exclaim");
                assert_eq!(tile_item.sources.len(), 1);
                match &tile_item.sources[0].source {
                    InputSource::ItemOutput {
                        item_index,
                        output_index,
                    } => {
                        assert_eq!(*item_index, 0);
                        assert_eq!(*output_index, 0);
                    }
                    _ => panic!("Expected ItemOutput"),
                }
            }
            _ => panic!("Expected Tile item"),
        }
    }
}
