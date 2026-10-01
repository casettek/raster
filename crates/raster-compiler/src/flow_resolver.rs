//! Data flow resolution for sequences.
//!
//! This module resolves how data flows between tiles in a sequence by tracking
//! variable bindings and mapping them to `InputSource` references.

use raster_core::cfs::{
    InputBinding, RecurSequenceItem, RecurTileItem, SequenceChildItem, SequenceItem,
    SequenceReturn, TileItem,
};
use raster_core::input::SelectorSegment;
use std::collections::{HashMap, HashSet};

use crate::ast::{CallArgumentKind, CallInfo, CallKind, ReturnExpr};
use crate::sequence::Sequence;

/// The declaration a recur site's `output` produces, with its schema left for
/// the CFS builder: it resolves the site's output type against the project's
/// structs (`CfsBuilder::fill_site_output_schemas`), which the resolver does
/// not hold.
fn site_output_decl(output: Option<crate::ast::SiteOutputKind>) -> Option<raster_core::cfs::RecurOutputDecl> {
    output.map(|kind| raster_core::cfs::RecurOutputDecl {
        schema_hash: [0u8; 32],
        empty_root: [0u8; 32],
        derives: kind == crate::ast::SiteOutputKind::Derive,
    })
}

/// Resolves data flow within a sequence, producing `SequenceItem`s with
/// correctly bound input sources.
#[derive(Default)]
pub struct FlowResolver {
    /// Map of variable names to the item index that produced them.
    bindings: HashMap<String, usize>,
    /// Sequence parameter names mapped to their input index.
    param_indices: HashMap<String, usize>,
    /// `main`'s entry-argument names — resolved to `InputBinding::EntryArgument`.
    entry_arguments: HashSet<String>,
    /// `let name = select!(T, root...)` locals, mapping `name` to `root`.
    selection_aliases: HashMap<String, String>,
    /// `let name = select!(T, rows[idx])` locals, mapping `name` to the names
    /// supplying its data-sourced indexes, in selector order.
    selection_index_sources: HashMap<String, Vec<String>>,
    /// Per alias local, the static selector path it appends to its root.
    selection_paths: HashMap<String, Vec<SelectorSegment>>,
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
    /// entry arguments. Every declared name resolves to
    /// `InputBinding::EntryArgument` — reached from the single authorized
    /// entry object at coordinates `[]` that the `ProgramStart` step binds —
    /// instead of `SequenceScope`, since `main` has no caller to supply them.
    /// There is no synthetic leading item, so item indices are not offset.
    pub fn resolve_with_entry_arguments(
        &mut self,
        sequence: &Sequence<'_>,
        entry_argument_names: &[String],
    ) -> Vec<SequenceChildItem> {
        // Reset state for this sequence
        self.bindings.clear();
        self.param_indices.clear();
        self.entry_arguments.clear();
        self.selection_aliases = sequence
            .function
            .selection_aliases
            .iter()
            .cloned()
            .collect();
        self.selection_index_sources = sequence
            .function
            .selection_index_sources
            .iter()
            .cloned()
            .collect();
        self.selection_paths = sequence
            .function
            .selection_paths
            .iter()
            .cloned()
            .collect();

        if entry_argument_names.is_empty() {
            // Map sequence parameters to their indices
            for (idx, name) in sequence.function.input_names.iter().enumerate() {
                self.param_indices.insert(name.clone(), idx);
            }
        } else {
            for name in entry_argument_names {
                self.entry_arguments.insert(name.clone());
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

        for (item_index, call) in relevant_calls.iter().enumerate() {
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
                    chunk: call.chunk,
                    output: site_output_decl(call.output),
                    state_is_output: call.state_is_output,
                }),
                CallKind::RecursiveSequence => {
                    SequenceChildItem::RecurSequence(RecurSequenceItem {
                        id: call.callee.clone(),
                        sources: input_sources,
                        state_is_output: call.state_is_output,
                        output: site_output_decl(call.output),
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

    /// Resolve what a sequence body returns, the way an argument is resolved.
    ///
    /// Call after [`Self::resolve_with_entry_arguments`], which fills the
    /// bindings this reads; `item_count` is the number of items it produced.
    /// `None` when the return cannot be bound: an [`ReturnExpr::Unbound`]
    /// form, or a name with no upstream (a finalized draft, a local computed
    /// in the body). A returned value selected through a data-sourced index is
    /// also `None` — the binding would need the index citations, and a
    /// program output has nowhere to carry them.
    pub fn resolve_return(&self, ret: &ReturnExpr, item_count: usize) -> Option<SequenceReturn> {
        let (source, path) = match ret {
            ReturnExpr::TailCall => (
                InputBinding::prior_item_output(item_count.checked_sub(1)?),
                Vec::new(),
            ),
            ReturnExpr::Rooted { root, path } => {
                let source = self.resolve_argument(&CallArgumentKind::Rooted { root: root.clone() });
                let (final_root, mut full_path) = self.compose_alias_path(root, path)?;
                // An entry argument is a field of the one entry object at `[]`,
                // and the runtime binds it with its name as the selector prefix
                // (`entry_argument_auth_ref`).
                if matches!(source.value_binding(), InputBinding::EntryArgument) {
                    full_path.insert(0, SelectorSegment::Field(final_root));
                }
                (source, full_path)
            }
            ReturnExpr::Unbound { .. } => return None,
        };
        let unbound = matches!(
            source.value_binding(),
            InputBinding::Direct(raster_core::cfs::InputSource::Inline)
        ) || !source.index_bindings().is_empty();
        (!unbound).then_some(SequenceReturn { source, path })
    }

    /// Walk `root`'s alias chain the way [`Self::resolve_alias`] does,
    /// prefixing each alias's static path: `let acc = select!(A, stats.w);
    /// let m = select!(u64, acc.max); m` composes to `[w, max]`, which is the
    /// selector the runtime builds by appending each `select!`'s segments to
    /// its base's. Returns the chain's root and the composed path, or `None`
    /// when an alias on the chain has no static path (a data-sourced index).
    fn compose_alias_path(
        &self,
        root: &str,
        path: &[SelectorSegment],
    ) -> Option<(String, Vec<SelectorSegment>)> {
        let mut current = root;
        let mut full_path = path.to_vec();
        for _ in 0..self.selection_aliases.len() {
            let Some(next) = self.selection_aliases.get(current) else {
                break;
            };
            let prefix = self.selection_paths.get(current)?;
            full_path.splice(0..0, prefix.iter().cloned());
            // A self-alias (`let x = select!(T, x.f)`) shadows: its path
            // applies once and the chain ends, as in `resolve_alias`.
            if next == current {
                break;
            }
            current = next.as_str();
        }
        Some((current.to_string(), full_path))
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
        let name = root.trim();
        // Collected before aliasing collapses the chain: the citations belong to
        // the selection locals along it, not to the root the chain lands on.
        let indexes = self.resolve_index_bindings(name);
        let root = self.resolve_alias(name);

        // A sequence parameter: supplied by the caller's scope.
        if let Some(&idx) = self.param_indices.get(root) {
            return InputBinding::seq_input(idx).indexed_by(indexes);
        }

        // One of `main`'s entry arguments: reached from the authorized entry
        // object at coordinates `[]` bound by the `ProgramStart` step.
        if self.entry_arguments.contains(root) {
            return InputBinding::entry_argument().indexed_by(indexes);
        }

        // A value produced by an earlier item of this sequence.
        if let Some(&item_index) = self.bindings.get(root) {
            return InputBinding::prior_item_output(item_index).indexed_by(indexes);
        }

        // A local with no upstream: materialized in the body.
        InputBinding::inline().indexed_by(indexes)
    }

    /// Resolve the index suppliers cited along `name`'s selection-alias chain.
    ///
    /// Walks the same chain `resolve_alias` walks, accumulating each link's
    /// data-sourced indexes, so a value reached through several selects carries
    /// every citation those selects made. Each supplier name is resolved to an
    /// ordinary `InputBinding` — the index is itself a value with provenance,
    /// which is exactly what the schema should record about it.
    ///
    /// The iteration bound mirrors `resolve_alias`: shadowing makes
    /// self-referential aliases ordinary, and following them unboundedly would
    /// not terminate.
    fn resolve_index_bindings(&self, name: &str) -> Vec<InputBinding> {
        let mut indexes = Vec::new();
        let mut current = name;
        for _ in 0..=self.selection_aliases.len() {
            if let Some(sources) = self.selection_index_sources.get(current) {
                for source in sources {
                    indexes.push(self.resolve_argument(&CallArgumentKind::Rooted {
                        root: source.clone(),
                    }));
                }
            }
            match self.selection_aliases.get(current) {
                Some(root) if root != current => current = root.as_str(),
                _ => break,
            }
        }
        indexes
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
                structs: vec![],
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
            selection_index_sources: vec![],
            selection_paths: vec![],
            return_expr: None,
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
        make_sequence_function_with_indexes(name, input_names, call_infos, selection_aliases, vec![])
    }

    fn make_sequence_function_with_indexes(
        name: &str,
        input_names: Vec<&str>,
        call_infos: Vec<CallInfo>,
        selection_aliases: Vec<(String, String)>,
        selection_index_sources: Vec<(String, Vec<String>)>,
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
            selection_index_sources,
            selection_paths: vec![],
            return_expr: None,
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
                    chunk: None,
                    output: None,
                    state_is_output: false,
                },
                CallInfo {
                    callee: "exclaim".to_string(),
                    result_binding: None,
                    arguments: vec!["greeting".to_string()],
                    argument_kinds: vec![CallArgumentKind::Rooted {
                        root: "greeting".to_string(),
                    }],
                    call_kind: CallKind::Tile,
                    chunk: None,
                    output: None,
                    state_is_output: false,
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

        // What the body returns binds like an argument: a binding to its
        // producing item, a tail call to the last item, a parameter to its
        // scope slot; a name with no upstream, or an unbindable form, to
        // nothing.
        let rooted = |root: &str, path: Vec<SelectorSegment>| ReturnExpr::Rooted {
            root: root.to_string(),
            path,
        };
        let returned = |source: InputBinding, path: Vec<SelectorSegment>| {
            Some(SequenceReturn { source, path })
        };
        assert_eq!(
            resolver.resolve_return(&rooted("greeting", vec![]), items.len()),
            returned(InputBinding::prior_item_output(0), vec![])
        );
        assert_eq!(
            resolver.resolve_return(
                &rooted("greeting", vec![SelectorSegment::Field("text".into())]),
                items.len()
            ),
            returned(
                InputBinding::prior_item_output(0),
                vec![SelectorSegment::Field("text".into())]
            )
        );
        assert_eq!(
            resolver.resolve_return(&ReturnExpr::TailCall, items.len()),
            returned(InputBinding::prior_item_output(1), vec![])
        );
        assert_eq!(
            resolver.resolve_return(&rooted("name", vec![]), items.len()),
            returned(InputBinding::seq_input(0), vec![])
        );
        assert_eq!(resolver.resolve_return(&rooted("report", vec![]), items.len()), None);
        assert_eq!(
            resolver.resolve_return(
                &ReturnExpr::Unbound {
                    expr: "42".to_string()
                },
                items.len()
            ),
            None
        );
    }

    /// A returned value's path is composed along its alias chain the way the
    /// runtime appends each `select!`'s segments to its base's selector, and an
    /// entry argument's path starts with the argument's name.
    #[test]
    fn return_paths_compose_along_the_alias_chain() {
        let project = make_mock_project();
        let summarize = make_tile_function("summarize", vec!["values"], true);
        let tile = Tile {
            function: &summarize,
            tile_type: "tile".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![tile],
        };
        let mut seq_func = make_sequence_function(
            "main",
            vec!["cfg"],
            vec![CallInfo {
                callee: "summarize".to_string(),
                result_binding: Some("stats".to_string()),
                arguments: vec!["cfg".to_string()],
                argument_kinds: vec![CallArgumentKind::Rooted {
                    root: "cfg".to_string(),
                }],
                call_kind: CallKind::Tile,
                chunk: None,
                output: None,
                state_is_output: false,
            }],
        );
        seq_func.selection_aliases = vec![
            ("window".to_string(), "stats".to_string()),
            ("m".to_string(), "window".to_string()),
            ("limit".to_string(), "cfg".to_string()),
        ];
        seq_func.selection_paths = vec![
            ("window".to_string(), vec![SelectorSegment::Field("window".into())]),
            ("m".to_string(), vec![SelectorSegment::Field("max".into())]),
            ("limit".to_string(), vec![SelectorSegment::Field("limit".into())]),
        ];
        let sequence = Sequence {
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };
        let mut resolver = FlowResolver::new();
        let items = resolver.resolve_with_entry_arguments(&sequence, &["cfg".to_string()]);

        let rooted = |root: &str| ReturnExpr::Rooted {
            root: root.to_string(),
            path: vec![],
        };
        assert_eq!(
            resolver.resolve_return(&rooted("m"), items.len()),
            Some(SequenceReturn {
                source: InputBinding::prior_item_output(0),
                path: vec![
                    SelectorSegment::Field("window".into()),
                    SelectorSegment::Field("max".into())
                ],
            })
        );
        assert_eq!(
            resolver.resolve_return(&rooted("limit"), items.len()),
            Some(SequenceReturn {
                source: InputBinding::entry_argument(),
                path: vec![
                    SelectorSegment::Field("cfg".into()),
                    SelectorSegment::Field("limit".into())
                ],
            })
        );
        assert_eq!(
            resolver.resolve_return(&rooted("cfg"), items.len()),
            Some(SequenceReturn {
                source: InputBinding::entry_argument(),
                path: vec![SelectorSegment::Field("cfg".into())],
            })
        );
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
                chunk: None,
                output: None,
                state_is_output: false,
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
    fn resolve_with_entry_arguments_binds_names_to_entry_argument_without_offset() {
        // There is no synthetic leading item, so entry-argument names bind
        // to `EntryArgument` and every real item keeps its natural index.
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
                    chunk: None,
                    output: None,
                    state_is_output: false,
                },
                CallInfo {
                    callee: "exclaim".to_string(),
                    result_binding: None,
                    arguments: vec!["greeting".to_string()],
                    argument_kinds: vec![CallArgumentKind::Rooted {
                        root: "greeting".to_string(),
                    }],
                    call_kind: CallKind::Tile,
                    chunk: None,
                    output: None,
                    state_is_output: false,
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

        // greet(personal_data): personal_data is an entry argument, bound to
        // `EntryArgument`, not SequenceScope.
        match &items[0] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "greet");
                match &tile_item.sources[0] {
                    InputBinding::EntryArgument => {}
                    other => panic!("Expected EntryArgument, got {:?}", other),
                }
            }
            _ => panic!("Expected Tile item"),
        }

        // exclaim(greeting): greeting was produced by items[0], whose schema
        // position is 0 now that there is no prepended item.
        match &items[1] {
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
                chunk: None,
                output: None,
                state_is_output: false,
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
                InputBinding::EntryArgument => {}
                other => panic!("Expected EntryArgument, got {:?}", other),
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
                chunk: None,
                output: None,
                state_is_output: false,
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
                InputBinding::EntryArgument => {}
                other => panic!("Expected EntryArgument, got {:?}", other),
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
                chunk: None,
                output: None,
                state_is_output: false,
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

    /// A `select!` whose index came from an authorized value must resolve to an
    /// `Indexed` binding wrapping the value's own provenance, with the index
    /// resolved to *its* provenance. This is what makes "reads the element named
    /// by binding X" a different program from "reads element 7" — the schema is
    /// hashed into program identity, and the guest pairs the declared index
    /// count against the recorded one.
    #[test]
    fn dynamic_index_resolves_to_an_indexed_binding() {
        let project = make_mock_project();
        let echo_func = make_tile_function("echo", vec!["row"], true);
        let echo_tile = Tile {
            function: &echo_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![echo_tile],
        };

        // Models:
        //   fn main(table) { let wanted = select!(u32, table.wanted);
        //                    let row = select!(Row, table.rows[wanted]);
        //                    call!(echo, row) }
        let seq_func = make_sequence_function_with_indexes(
            "main",
            vec!["table"],
            vec![CallInfo {
                callee: "echo".to_string(),
                result_binding: None,
                arguments: vec!["row".to_string()],
                argument_kinds: vec![CallArgumentKind::Rooted {
                    root: "row".to_string(),
                }],
                call_kind: CallKind::Tile,
                chunk: None,
                output: None,
                state_is_output: false,
            }],
            vec![
                ("row".to_string(), "table".to_string()),
                ("wanted".to_string(), "table".to_string()),
            ],
            vec![("row".to_string(), vec!["wanted".to_string()])],
        );
        let sequence = Sequence {
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };

        let mut resolver = FlowResolver::new();
        let items = resolver.resolve(&sequence);

        let SequenceChildItem::Tile(tile_item) = &items[0] else {
            panic!("Expected Tile item");
        };
        let InputBinding::Indexed { value, indexes } = &tile_item.sources[0] else {
            panic!(
                "expected an Indexed binding, got {:?}",
                tile_item.sources[0]
            );
        };
        // The row still comes from the caller's scope; only its *index* is new.
        assert!(matches!(
            value.as_ref(),
            InputBinding::SequenceScope { input_index: 0 }
        ));
        // And the index is itself a value with provenance, recorded as such.
        assert_eq!(indexes.len(), 1);
        assert!(matches!(
            &indexes[0],
            InputBinding::SequenceScope { input_index: 0 }
        ));
    }

    /// A literal-index selection must keep emitting exactly the binding it emits
    /// today, so no existing program's identity shifts under this feature.
    #[test]
    fn literal_index_resolves_without_a_wrapper() {
        let project = make_mock_project();
        let echo_func = make_tile_function("echo", vec!["row"], true);
        let echo_tile = Tile {
            function: &echo_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let tile_discovery = TileDiscovery {
            project: &project,
            tiles: vec![echo_tile],
        };

        let seq_func = make_sequence_function_with_aliases(
            "main",
            vec!["table"],
            vec![CallInfo {
                callee: "echo".to_string(),
                result_binding: None,
                arguments: vec!["row".to_string()],
                argument_kinds: vec![CallArgumentKind::Rooted {
                    root: "row".to_string(),
                }],
                call_kind: CallKind::Tile,
                chunk: None,
                output: None,
                state_is_output: false,
            }],
            vec![("row".to_string(), "table".to_string())],
        );
        let sequence = Sequence {
            function: &seq_func,
            steps: vec![SequenceStep::Tile(&tile_discovery.tiles[0])],
            description: None,
        };

        let mut resolver = FlowResolver::new();
        let items = resolver.resolve(&sequence);

        let SequenceChildItem::Tile(tile_item) = &items[0] else {
            panic!("Expected Tile item");
        };
        assert!(matches!(
            &tile_item.sources[0],
            InputBinding::SequenceScope { input_index: 0 }
        ));
    }
}
