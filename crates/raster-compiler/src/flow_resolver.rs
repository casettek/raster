//! Data flow resolution for sequences.
//!
//! This module resolves how data flows between tiles in a sequence by tracking
//! variable bindings and mapping them to `InputSource` references.

use raster_core::cfs::{InputBinding, InputSource, SequenceItem};
use std::collections::HashMap;

use crate::discovery::{DiscoveredSequence, DiscoveredTile, SequenceCall};

/// Resolves data flow within a sequence, producing `SequenceItem`s with
/// correctly bound input sources.
pub struct FlowResolver<'a> {
    /// Map of variable names to their source (item_index, output_index).
    bindings: HashMap<String, (usize, usize)>,
    /// Sequence parameter names mapped to their input index.
    param_indices: HashMap<String, usize>,
    /// Known tiles for looking up output counts.
    tiles: &'a [DiscoveredTile],
    /// Known sequences for looking up output counts (for nested sequences).
    sequences: &'a [DiscoveredSequence],
}

impl<'a> FlowResolver<'a> {
    /// Create a new flow resolver.
    pub fn new(tiles: &'a [DiscoveredTile], sequences: &'a [DiscoveredSequence]) -> Self {
        Self {
            bindings: HashMap::new(),
            param_indices: HashMap::new(),
            tiles,
            sequences,
        }
    }

    /// Resolve a discovered sequence into a list of `SequenceItem`s with input sources.
    pub fn resolve(&mut self, sequence: &DiscoveredSequence) -> Vec<SequenceItem> {
        // Reset state for this sequence
        self.bindings.clear();
        self.param_indices.clear();

        // Map sequence parameters to their indices
        for (idx, name) in sequence.param_names.iter().enumerate() {
            self.param_indices.insert(name.clone(), idx);
        }

        let mut items = Vec::new();

        for (item_index, call) in sequence.calls.iter().enumerate() {
            let input_sources = self.resolve_call_inputs(call);

            // Determine if this is a tile or nested sequence
            let item_type = if self.is_tile(&call.callee) {
                "tile"
            } else if self.is_sequence(&call.callee) {
                "sequence"
            } else {
                // Assume it's a tile if we can't determine
                "tile"
            };

            let item = SequenceItem {
                item_type: item_type.to_string(),
                item_id: call.callee.clone(),
                input_sources,
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
    fn resolve_call_inputs(&self, call: &SequenceCall) -> Vec<InputBinding> {
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

    /// Check if a callee name is a known tile.
    fn is_tile(&self, name: &str) -> bool {
        self.tiles.iter().any(|t| t.metadata.id.0 == name)
    }

    /// Check if a callee name is a known sequence.
    fn is_sequence(&self, name: &str) -> bool {
        self.sequences.iter().any(|s| s.id == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::SequenceCall;
    use raster_core::tile::{TileId, TileMetadata};

    fn make_tile(id: &str, inputs: usize, outputs: usize) -> DiscoveredTile {
        DiscoveredTile {
            metadata: TileMetadata {
                id: TileId(id.to_string()),
                name: id.to_string(),
                description: None,
                estimated_cycles: None,
                max_memory: None,
            },
            source_file: "test.rs".to_string(),
            line_number: 1,
            input_count: inputs,
            output_count: outputs,
            tile_type: "iter".to_string(),
        }
    }

    fn make_sequence(id: &str, param_names: Vec<&str>, calls: Vec<SequenceCall>) -> DiscoveredSequence {
        let input_count = param_names.len();
        DiscoveredSequence {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            calls,
            param_names: param_names.into_iter().map(|s| s.to_string()).collect(),
            input_count,
            source_file: "test.rs".to_string(),
            line_number: 1,
        }
    }

    #[test]
    fn test_resolve_simple_sequence() {
        let tiles = vec![
            make_tile("greet", 1, 1),
            make_tile("exclaim", 1, 1),
        ];
        let sequences = vec![];

        let seq = make_sequence(
            "main",
            vec!["name"],
            vec![
                SequenceCall {
                    callee: "greet".to_string(),
                    result_binding: Some("greeting".to_string()),
                    arguments: vec!["name".to_string()],
                },
                SequenceCall {
                    callee: "exclaim".to_string(),
                    result_binding: None,
                    arguments: vec!["greeting".to_string()],
                },
            ],
        );

        let mut resolver = FlowResolver::new(&tiles, &sequences);
        let items = resolver.resolve(&seq);

        assert_eq!(items.len(), 2);

        // First item: greet(name) where name is seq_input[0]
        assert_eq!(items[0].item_id, "greet");
        assert_eq!(items[0].item_type, "tile");
        assert_eq!(items[0].input_sources.len(), 1);
        match &items[0].input_sources[0].source {
            InputSource::SeqInput { input_index } => assert_eq!(*input_index, 0),
            _ => panic!("Expected SeqInput"),
        }

        // Second item: exclaim(greeting) where greeting is item_output[0][0]
        assert_eq!(items[1].item_id, "exclaim");
        assert_eq!(items[1].item_type, "tile");
        assert_eq!(items[1].input_sources.len(), 1);
        match &items[1].input_sources[0].source {
            InputSource::ItemOutput { item_index, output_index } => {
                assert_eq!(*item_index, 0);
                assert_eq!(*output_index, 0);
            }
            _ => panic!("Expected ItemOutput"),
        }
    }
}

