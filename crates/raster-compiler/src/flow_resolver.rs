//! Data flow resolution for sequences.
//!
//! This module resolves how data flows between tiles in a sequence by tracking
//! variable bindings and mapping them to `InputSource` references.

use raster_core::cfs::{
    InputBinding, RecurSequenceItem, RecurTileItem, SequenceChildItem, SequenceItem, TileItem,
};
use std::collections::HashMap;

use crate::ast::{CallArgumentKind, CallInfo, CallKind};
use crate::sequence::Sequence;

/// Resolves data flow within a sequence, producing `SequenceItem`s with
/// correctly bound input sources.
#[derive(Default)]
pub struct FlowResolver {
    /// Map of variable names to the item index that produced them.
    bindings: HashMap<String, usize>,
    /// Sequence parameter names mapped to their input index.
    param_indices: HashMap<String, usize>,
    /// `let name = select!(T, root...)` locals, mapping `name` to `root`.
    selection_aliases: HashMap<String, String>,
}

impl FlowResolver {
    /// Create a new flow resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a discovered sequence into a list of `SequenceChild`s with input sources.
    ///
    /// Equivalent to `resolve_with_entry_arguments(sequence, &[])` — every
    /// non-entrypoint sequence (including `main` when it declares no
    /// arguments) resolves exactly as before.
    pub fn resolve(&mut self, sequence: &Sequence<'_>) -> Vec<SequenceChildItem> {
        self.resolve_with_entry_arguments(sequence, &[])
    }

    /// Resolve a sequence that declares `entry_argument_names` as `main`
    /// entry arguments (see `SequenceChildItem::Entrypoint`). Every declared
    /// name binds to item index `0` — the leading `Entrypoint` item the
    /// caller (`CfsBuilder`) prepends separately — instead of to
    /// `SequenceScope`, since `main` has no caller to supply them. Every
    /// other resolved item's index is offset by one to leave that slot for
    /// the `Entrypoint` item.
    pub fn resolve_with_entry_arguments(
        &mut self,
        sequence: &Sequence<'_>,
        entry_argument_names: &[String],
    ) -> Vec<SequenceChildItem> {
        // Reset state for this sequence
        self.bindings.clear();
        self.param_indices.clear();
        self.selection_aliases = sequence
            .function
            .selection_aliases
            .iter()
            .cloned()
            .collect();

        let item_index_offset = if entry_argument_names.is_empty() {
            0
        } else {
            1
        };

        if entry_argument_names.is_empty() {
            // Map sequence parameters to their indices
            for (idx, name) in sequence.function.input_names.iter().enumerate() {
                self.param_indices.insert(name.clone(), idx);
            }
        } else {
            // All entry arguments live at the single Entrypoint item.
            for name in entry_argument_names {
                self.bindings.insert(name.clone(), 0);
            }
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
                crate::sequence::SequenceStep::RecurTile(tile) => tile.function.name.as_str(),
                crate::sequence::SequenceStep::RecurSequence(name) => name.as_str(),
                crate::sequence::SequenceStep::Sequence(name) => name.as_str(),
            })
            .collect();

        let relevant_calls: Vec<&CallInfo> = sequence
            .function
            .call_infos
            .iter()
            .filter(|call| step_callees.contains(&call.callee.as_str()))
            .collect();

        for (call_index, call) in relevant_calls.iter().enumerate() {
            let item_index = call_index + item_index_offset;
            let input_sources = self.resolve_call_inputs(call);

            // Call kind directly determines item type — no name-matching needed.
            let item = match call.call_kind {
                CallKind::Tile => SequenceChildItem::Tile(TileItem {
                    id: call.callee.clone(),
                    sources: input_sources,
                }),
                CallKind::RecursiveTile => SequenceChildItem::RecurTile(RecurTileItem {
                    id: call.callee.clone(),
                    sources: input_sources,
                }),
                CallKind::RecursiveSequence => {
                    SequenceChildItem::RecurSequence(RecurSequenceItem {
                        id: call.callee.clone(),
                        sources: input_sources,
                    })
                }
                CallKind::Sequence => SequenceChildItem::Sequence(SequenceItem {
                    id: call.callee.clone(),
                    sources: input_sources,
                }),
            };

            items.push(item);

            // If this call has a result binding, record it
            if let Some(ref binding_name) = call.result_binding {
                self.bindings.insert(binding_name.clone(), item_index);
            }
        }

        items
    }

    /// Resolve input sources for a function call's arguments.
    fn resolve_call_inputs(&self, call: &CallInfo) -> Vec<InputBinding> {
        call.argument_kinds
            .iter()
            .map(|kind| self.resolve_argument(kind))
            .collect()
    }

    /// Follow `let x = select!(T, y...)` chains back to the name that
    /// actually carries provenance.
    ///
    /// The iteration bound is load-bearing, not defensive: shadowing makes
    /// self-referential aliases ordinary. `let seed = select!(u64, seed)`
    /// records `seed -> seed`, where the right-hand `seed` is the entry
    /// argument and the left-hand one is the narrowed local. Following that
    /// unboundedly would not terminate; following it a bounded number of
    /// times lands on the same name either way, which is the right answer.
    fn resolve_alias<'a>(&'a self, name: &'a str) -> &'a str {
        let mut current = name;
        for _ in 0..self.selection_aliases.len() {
            match self.selection_aliases.get(current) {
                Some(root) if root != current => current = root.as_str(),
                _ => break,
            }
        }
        current
    }

    /// Resolve a single argument to its input source.
    ///
    /// Everything reachable from a name — a sequence parameter, a prior
    /// item's output, an entry argument — binds to that name's source, so
    /// that the guest can hold the recorded step to it. Only values with no
    /// upstream at all are `Inline`.
    fn resolve_argument(&self, kind: &CallArgumentKind) -> InputBinding {
        let CallArgumentKind::Rooted { root } = kind else {
            return InputBinding::inline();
        };
        let root = self.resolve_alias(root.trim());

        // A sequence parameter: supplied by the caller's scope.
        if let Some(&idx) = self.param_indices.get(root) {
            return InputBinding::seq_input(idx);
        }

        // A value produced by an earlier item of this sequence — including
        // `main`'s entry arguments, which all bind to the leading Entrypoint
        // item.
        if let Some(&item_index) = self.bindings.get(root) {
            return InputBinding::prior_item_output(item_index);
        }

        // A local with no upstream: materialized in the body.
        InputBinding::inline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionAstItem;
    use crate::ast::MacroAstItem;
    use crate::ast::ProjectAst;
    use crate::sequence::SequenceStep;
    use crate::tile::{Tile, TileDiscovery};
    use crate::Project;
    use raster_core::cfs::InputSource;
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
            selection_aliases: vec![],
        }
    }

    fn make_sequence_function(
        name: &str,
        input_names: Vec<&str>,
        call_infos: Vec<CallInfo>,
    ) -> FunctionAstItem {
        make_sequence_function_with_aliases(name, input_names, call_infos, vec![])
    }

    fn make_sequence_function_with_aliases(
        name: &str,
        input_names: Vec<&str>,
        call_infos: Vec<CallInfo>,
        selection_aliases: Vec<(String, String)>,
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
            selection_aliases,
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

        // Create sequence function with call infos
        let seq_func = make_sequence_function(
            "main",
            vec!["name"],
            vec![
                CallInfo {
                    callee: "greet".to_string(),
                    result_binding: Some("greeting".to_string()),
                    arguments: vec!["name".to_string()],
                    argument_kinds: vec![CallArgumentKind::Rooted {
                        root: "name".to_string(),
                    }],
                    call_kind: CallKind::Tile,
                },
                CallInfo {
                    callee: "exclaim".to_string(),
                    result_binding: None,
                    arguments: vec!["greeting".to_string()],
                    argument_kinds: vec![CallArgumentKind::Rooted {
                        root: "greeting".to_string(),
                    }],
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

        let mut resolver = FlowResolver::new();
        let items = resolver.resolve(&sequence);

        assert_eq!(items.len(), 2);

        // First item: greet(name) where name is seq_input[0]
        match &items[0] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "greet");
                assert_eq!(tile_item.sources.len(), 1);
                match &tile_item.sources[0] {
                    InputBinding::SequenceScope { input_index } => assert_eq!(*input_index, 0),
                    _ => panic!("Expected SequenceScope"),
                }
            }
            _ => panic!("Expected Tile item"),
        }

        // Second item: exclaim(greeting) where greeting is item 0's output
        match &items[1] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "exclaim");
                assert_eq!(tile_item.sources.len(), 1);
                match &tile_item.sources[0] {
                    InputBinding::PriorItemOutput {
                        intra_sequence_item_index,
                    } => {
                        assert_eq!(*intra_sequence_item_index, 0);
                    }
                    _ => panic!("Expected PriorItemOutput"),
                }
            }
            _ => panic!("Expected Tile item"),
        }
    }

    #[test]
    fn test_resolve_inline_argument_as_inline_source() {
        let project = make_mock_project();
        let greet_func = make_tile_function("greet", vec!["name"], true);
        let tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![tile],
        };

        let seq_func = make_sequence_function(
            "main",
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
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };

        let mut resolver = FlowResolver::new();
        let items = resolver.resolve(&sequence);

        match &items[0] {
            SequenceChildItem::Tile(tile_item) => match &tile_item.sources[0] {
                InputBinding::Direct(InputSource::Inline) => {}
                other => panic!("Expected Inline source, got {:?}", other),
            },
            _ => panic!("Expected Tile item"),
        }
    }

    #[test]
    fn resolve_with_entry_arguments_binds_names_to_item_zero_and_offsets_the_rest() {
        // The Entrypoint item itself is built by CfsBuilder, not the
        // resolver — the resolver only needs to know the declared names so
        // it can bind them, and that every other item's index leaves room
        // for it at position 0.
        let project = make_mock_project();
        let greet_func = make_tile_function("greet", vec!["input"], true);
        let exclaim_func = make_tile_function("exclaim", vec!["input"], true);
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
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

        // `main`'s own `input_names` no longer matter for entry-argument
        // resolution — the caller passes them explicitly — so leave them
        // empty here to prove that.
        let seq_func = make_sequence_function(
            "main",
            vec![],
            vec![
                CallInfo {
                    callee: "greet".to_string(),
                    result_binding: Some("greeting".to_string()),
                    arguments: vec!["personal_data".to_string()],
                    argument_kinds: vec![CallArgumentKind::Rooted {
                        root: "personal_data".to_string(),
                    }],
                    call_kind: CallKind::Tile,
                },
                CallInfo {
                    callee: "exclaim".to_string(),
                    result_binding: None,
                    arguments: vec!["greeting".to_string()],
                    argument_kinds: vec![CallArgumentKind::Rooted {
                        root: "greeting".to_string(),
                    }],
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

        let mut resolver = FlowResolver::new();
        let entry_names = vec!["personal_data".to_string(), "seed".to_string()];
        let items = resolver.resolve_with_entry_arguments(&sequence, &entry_names);

        assert_eq!(items.len(), 2);

        // greet(personal_data): personal_data is an entry argument, bound
        // to item index 0 (the Entrypoint item), not SequenceScope.
        match &items[0] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "greet");
                match &tile_item.sources[0] {
                    InputBinding::PriorItemOutput {
                        intra_sequence_item_index,
                    } => assert_eq!(*intra_sequence_item_index, 0),
                    other => panic!("Expected PriorItemOutput{{0}}, got {:?}", other),
                }
            }
            _ => panic!("Expected Tile item"),
        }

        // exclaim(greeting): greeting was produced by items[0] here, whose
        // *overall* schema position is 1 (item 0 is the Entrypoint item
        // CfsBuilder prepends), so the offset must land on 1, not 0.
        match &items[1] {
            SequenceChildItem::Tile(tile_item) => match &tile_item.sources[0] {
                InputBinding::PriorItemOutput {
                    intra_sequence_item_index,
                } => assert_eq!(*intra_sequence_item_index, 1),
                other => panic!("Expected PriorItemOutput{{1}}, got {:?}", other),
            },
            _ => panic!("Expected Tile item"),
        }
    }

    #[test]
    fn selected_entry_arguments_bind_to_their_source_not_to_inline() {
        // `let name = select!(String, personal_data.name); call!(greet, name);`
        // — `name` is committed data reached through a selection, so it must
        // bind to the entry-argument item. Binding it as `Inline` would let a
        // claimed trace substitute arbitrary bytes for it and still verify.
        let project = make_mock_project();
        let greet_func = make_tile_function("greet", vec!["input"], true);
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![greet_tile],
        };

        let seq_func = make_sequence_function_with_aliases(
            "main",
            vec![],
            vec![CallInfo {
                callee: "greet".to_string(),
                result_binding: None,
                arguments: vec!["name".to_string()],
                argument_kinds: vec![CallArgumentKind::Rooted {
                    root: "name".to_string(),
                }],
                call_kind: CallKind::Tile,
            }],
            vec![("name".to_string(), "personal_data".to_string())],
        );
        let sequence = Sequence {
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };

        let mut resolver = FlowResolver::new();
        let items =
            resolver.resolve_with_entry_arguments(&sequence, &["personal_data".to_string()]);

        match &items[0] {
            SequenceChildItem::Tile(tile_item) => match &tile_item.sources[0] {
                InputBinding::PriorItemOutput {
                    intra_sequence_item_index,
                } => assert_eq!(*intra_sequence_item_index, 0),
                other => panic!("Expected PriorItemOutput{{0}}, got {:?}", other),
            },
            _ => panic!("Expected Tile item"),
        }
    }

    #[test]
    fn self_shadowing_selection_aliases_resolve_to_the_entry_argument() {
        // `let seed = select!(u64, seed);` — the local shadows the entry
        // argument it narrows, recording `seed -> seed`. It must still bind
        // to the entry argument (and must terminate).
        let project = make_mock_project();
        let greet_func = make_tile_function("greet", vec!["input"], true);
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![greet_tile],
        };

        let seq_func = make_sequence_function_with_aliases(
            "main",
            vec![],
            vec![CallInfo {
                callee: "greet".to_string(),
                result_binding: None,
                arguments: vec!["seed".to_string()],
                argument_kinds: vec![CallArgumentKind::Rooted {
                    root: "seed".to_string(),
                }],
                call_kind: CallKind::Tile,
            }],
            vec![("seed".to_string(), "seed".to_string())],
        );
        let sequence = Sequence {
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };

        let mut resolver = FlowResolver::new();
        let items = resolver.resolve_with_entry_arguments(&sequence, &["seed".to_string()]);

        match &items[0] {
            SequenceChildItem::Tile(tile_item) => match &tile_item.sources[0] {
                InputBinding::PriorItemOutput {
                    intra_sequence_item_index,
                } => assert_eq!(*intra_sequence_item_index, 0),
                other => panic!("Expected PriorItemOutput{{0}}, got {:?}", other),
            },
            _ => panic!("Expected Tile item"),
        }
    }

    #[test]
    fn locals_with_no_upstream_bind_as_inline() {
        let project = make_mock_project();
        let greet_func = make_tile_function("greet", vec!["input"], true);
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![greet_tile],
        };

        let seq_func = make_sequence_function(
            "main",
            vec![],
            vec![CallInfo {
                callee: "greet".to_string(),
                result_binding: None,
                arguments: vec!["\"Rust\".to_string()".to_string()],
                argument_kinds: vec![CallArgumentKind::Inline],
                call_kind: CallKind::Tile,
            }],
        );
        let sequence = Sequence {
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };

        let mut resolver = FlowResolver::new();
        let items = resolver.resolve(&sequence);

        match &items[0] {
            SequenceChildItem::Tile(tile_item) => assert!(matches!(
                &tile_item.sources[0],
                InputBinding::Direct(InputSource::Inline)
            )),
            _ => panic!("Expected Tile item"),
        }
    }
}
