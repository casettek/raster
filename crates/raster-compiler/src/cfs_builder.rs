//! Control Flow Schema (CFS) builder.
//!
//! This module orchestrates the generation of a CFS from a Raster project
//! by combining tile discovery, sequence discovery, and data flow resolution.

use raster_core::cfs::{ControlFlowSchema, InputBinding, SequenceDef, SequenceReturn, TileDef, SequenceChildItem};

use crate::flow_resolver::FlowResolver;
use crate::sequence::{Sequence, SequenceDiscovery};
use crate::tile::TileDiscovery;
use crate::Project;
use raster_core::{Error, Result};

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
        let mut tiles: Vec<TileDef> = tile_discovery
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
            let mut seq_def = self.build_sequence_def(seq)?;
            self.fill_site_output_schemas(&mut seq_def)?;
            sequences.push(seq_def);
        }

        // Canonicalize: the CFS is the control-flow half of a program's
        // identity (see docs/proposals/program-identity.md), so its byte form
        // must be reproducible regardless of the filesystem order in which
        // tiles and sequences were discovered (`WalkDir` does not sort). Sort
        // both by id and reject duplicate ids — a duplicate would make a name
        // ambiguous in the identity registry. `SequenceDef::items` order is
        // semantic (coordinates index into it) and is left untouched.
        tiles.sort_by(|a, b| a.id.cmp(&b.id));
        sequences.sort_by(|a, b| a.id.cmp(&b.id));
        assert_unique_ids("tile", tiles.iter().map(|t| t.id.as_str()))?;
        assert_unique_ids("sequence", sequences.iter().map(|s| s.id.as_str()))?;

        Ok(ControlFlowSchema {
            version: "1.0".to_string(),
            project: self.project.name.clone(),
            encoding: "postcard".to_string(),
            tiles,
            sequences,
        })
    }

    /// Fill each recur site's [`RecurOutputDecl`] with its output type's
    /// schema hash and empty root (D1 of `incremental-draft-materialization`):
    /// the object a site owns is declared by the program, not chosen by the
    /// prover.
    ///
    /// The type comes from the site's own signature — the `RecurOutput<S>`
    /// parameter of a recur tile, the `RecurSequenceOutput<S>` parameter of a
    /// recur sequence — and is resolved by the same `schema_walk` that fills
    /// the program interface's schema hashes.
    fn fill_site_output_schemas(&self, sequence: &mut SequenceDef) -> Result<()> {
        for item in &mut sequence.items {
            let (id, output, wrapper) = match item {
                SequenceChildItem::RecurTile(item) => (&item.id, &mut item.output, "RecurOutput"),
                SequenceChildItem::RecurSequence(item) => {
                    (&item.id, &mut item.output, "RecurSequenceOutput")
                }
                _ => continue,
            };
            let Some(declaration) = output.as_mut() else {
                continue;
            };
            let function = self
                .project
                .ast
                .functions
                .iter()
                .find(|function| &function.name == id)
                .ok_or_else(|| {
                    Error::Other(format!("Recur site '{id}' has no function to read its output type from"))
                })?;
            let output_type = function
                .inputs
                .iter()
                .find_map(|ty| generic_inner(ty, wrapper))
                .ok_or_else(|| {
                    Error::Other(format!(
                        "Recur site '{id}' has an `output` but no `{wrapper}<S>` parameter"
                    ))
                })?;
            let schema =
                crate::schema_walk::schema_of_type(&output_type, &self.project.ast.structs)?;
            declaration.schema_hash = raster_core::draft::schema_hash(&schema);
            declaration.empty_root = raster_core::draft::draft_root_from_field_roots(
                &schema,
                &std::collections::BTreeMap::new(),
            )?;
        }
        Ok(())
    }

    /// Build a sequence definition from a discovered sequence.
    ///
    /// `main`'s declared parameters are entry arguments, not caller-supplied
    /// `SequenceScope` parameters — main has no caller. When `main` declares
    /// any, they are recorded in `SequenceDef::entry_arguments` (bound at
    /// runtime by the program's `ProgramStart` step into one authorized
    /// object at coordinates `[]`), and every consuming item resolves them
    /// through `InputBinding::EntryArgument`. Every other sequence (including
    /// `main` with no declared parameters) is built exactly as before.
    fn build_sequence_def(&self, seq: &Sequence<'_>) -> Result<SequenceDef> {
        let mut resolver = FlowResolver::new();

        // Only `main` produces a program output, and only when it returns a
        // non-unit value — its declared return type is the program's output
        // declaration (bound at runtime by the `ProgramEnd` step).
        let produces_output = seq.function.name == "main" && returns_non_unit(&seq.function.output);

        if seq.function.name == "main" && !seq.function.input_names.is_empty() {
            let items = resolver.resolve_with_entry_arguments(seq, &seq.function.input_names);
            let returns = sequence_returns(&resolver, seq, items.len());

            return Ok(SequenceDef {
                id: seq.function.name.clone(),
                input_sources: Vec::new(),
                items,
                entry_arguments: seq.function.input_names.clone(),
                produces_output,
                returns,
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
        let returns = sequence_returns(&resolver, seq, items.len());

        Ok(SequenceDef {
            id: seq.function.name.clone(),
            input_sources,
            items,
            entry_arguments: Vec::new(),
            produces_output,
            returns,
        })
    }
}

/// Bind the value a sequence returns, so the guest can follow it.
///
/// Recorded for every sequence that returns a value, relative to the
/// sequence itself (an item index, never a coordinate — one definition is
/// called from many places). The guest walks these from `main` or from any
/// consumer of a sequence's output down to the step that wrote the object
/// (`CfsCursor::resolve_value`). A recur sequence's body is skipped: its site
/// writes the result at the site's own coordinate, which is where the walk
/// stops.
///
/// An unbindable return is not a build error — a program returning
/// `finalize(draft)` must still build and run unauthenticated — so it is
/// reported here rather than discovered at proving time: for `main`, its
/// `ProgramEnd` will not verify; for a nested sequence, a use of its result is
/// held only to "inside the call".
fn sequence_returns(
    resolver: &FlowResolver,
    seq: &Sequence<'_>,
    item_count: usize,
) -> Option<SequenceReturn> {
    if !returns_non_unit(&seq.function.output) || is_recur_sequence(seq) {
        return None;
    }
    let returns = seq
        .function
        .return_expr
        .as_ref()
        .and_then(|ret| resolver.resolve_return(ret, item_count));
    if returns.is_none() {
        let name = &seq.function.name;
        let form = match &seq.function.return_expr {
            Some(crate::ast::ReturnExpr::Unbound { expr }) => format!("`{expr}`"),
            Some(crate::ast::ReturnExpr::Rooted { root, .. }) => {
                format!("`{root}` (a name no step produced)")
            }
            Some(crate::ast::ReturnExpr::TailCall) => "its final call".to_string(),
            None => "no returned expression".to_string(),
        };
        let consequence = if name == "main" {
            "this program's ProgramEnd will not verify"
        } else {
            "a use of its result is held only to \"inside the call\""
        };
        eprintln!(
            "warning: `{name}` returns {form}, which the CFS cannot bind to a step's output or \
             an argument; {consequence}. Return a binding of a `call!`/`call_recur!`/`call_seq!` \
             result, or a `select!` of one."
        );
    }
    returns
}

/// Whether `seq` is the body of a recur sequence (`#[sequence(kind = recur)]`).
fn is_recur_sequence(seq: &Sequence<'_>) -> bool {
    seq.function.macros.iter().any(|attr| {
        attr.name.rsplit("::").next() == Some("sequence")
            && attr.args.get("kind").map(String::as_str) == Some("recur")
    })
}

/// Reject duplicate ids in a (already sorted) id sequence, naming the kind
/// (`tile`/`sequence`) and the offending id in the error.
fn assert_unique_ids<'a>(kind: &str, ids: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut prev: Option<&str> = None;
    for id in ids {
        if prev == Some(id) {
            return Err(Error::Other(format!(
                "duplicate {kind} id '{id}' — {kind} ids must be unique to form a program identity"
            )));
        }
        prev = Some(id);
    }
    Ok(())
}

/// Whether a return-type string denotes a non-unit value. `None` (no return
/// type) and unit — including `Result<()>` — are not outputs; every other
/// parseable type is. Mirrors the macro's `Unit`/`Value`/`Fallible` split
/// (see `raster-macros`), but from the parsed signature string.
fn returns_non_unit(output: &Option<String>) -> bool {
    let Some(output) = output else {
        return false;
    };
    match syn::parse_str::<syn::Type>(output) {
        Ok(ty) => !type_is_unit(&ty),
        // Unparseable here should not happen (it parsed once already); treat a
        // present return type conservatively as an output.
        Err(_) => true,
    }
}

fn type_is_unit(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Tuple(tuple) => tuple.elems.is_empty(),
        syn::Type::Path(type_path) => {
            let Some(segment) = type_path.path.segments.last() else {
                return false;
            };
            if segment.ident != "Result" {
                return false;
            }
            let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
                return false;
            };
            match args.args.first() {
                Some(syn::GenericArgument::Type(inner)) => type_is_unit(inner),
                _ => false,
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::SequenceChildItem;

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
                structs: vec![],
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
            selection_index_sources: vec![],
            selection_paths: vec![],
            return_expr: None,
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
            selection_index_sources: vec![],
            selection_paths: vec![],
            return_expr: None,
        }
    }

    #[test]
    fn main_with_entry_arguments_records_them_and_keeps_input_sources_empty() {
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
                chunk: None,
                output: None,
                state_is_output: false,
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
            "main's own input_sources must be empty once its parameters become entry arguments"
        );
        assert_eq!(
            seq_def.entry_arguments,
            vec!["personal_data".to_string(), "seed".to_string()],
            "entry arguments are recorded on the sequence def in declaration order",
        );
        assert_eq!(
            seq_def.items.len(),
            1,
            "no synthetic entrypoint item — the tile is the leading item at index 0"
        );

        match &seq_def.items[0] {
            SequenceChildItem::Tile(tile_item) => {
                assert_eq!(tile_item.id, "greet");
                match &tile_item.sources[0] {
                    InputBinding::EntryArgument => {}
                    other => panic!("Expected EntryArgument binding, got {:?}", other),
                }
            }
            other => panic!("Expected a Tile item, got {:?}", other),
        }
    }

    /// Every sequence that returns a value records what it returns — not only
    /// `main` — except a recur sequence's body, whose site writes the result
    /// at its own coordinate.
    #[test]
    fn nested_sequences_record_their_returns_and_recur_bodies_do_not() {
        let project = mock_project();
        let greet_func = tile_function("greet");
        let greet_tile = Tile {
            function: &greet_func,
            tile_type: "iter".to_string(),
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };
        let body = |name: &str, kind: Option<&str>| {
            let mut function = main_function_with_params(
                vec![],
                vec![CallInfo {
                    callee: "greet".to_string(),
                    result_binding: Some("greeting".to_string()),
                    arguments: vec!["\"hi\"".to_string()],
                    argument_kinds: vec![CallArgumentKind::Inline],
                    call_kind: CallKind::Tile,
                    chunk: None,
                    output: None,
                    state_is_output: false,
                }],
            );
            function.name = name.to_string();
            function.output = Some("String".to_string());
            function.return_expr = Some(crate::ast::ReturnExpr::Rooted {
                root: "greeting".to_string(),
                path: vec![],
            });
            if let Some(kind) = kind {
                function.macros[0].args.insert("kind".to_string(), kind.to_string());
            }
            function
        };
        let builder = CfsBuilder::new(&project);
        let def_of = |function: &FunctionAstItem| {
            builder
                .build_sequence_def(&Sequence {
                    function,
                    steps: vec![SequenceStep::Tile(&greet_tile)],
                    description: None,
                })
                .unwrap()
        };

        let nested = def_of(&body("helper", None));
        assert!(!nested.produces_output, "only main produces the program output");
        assert_eq!(
            nested.returns,
            Some(SequenceReturn {
                source: InputBinding::prior_item_output(0),
                path: vec![],
            })
        );
        assert_eq!(def_of(&body("sweep", Some("recur"))).returns, None);
    }

    fn project_with_tile_functions(names: &[&str]) -> Project {
        let functions = names
            .iter()
            .map(|name| tile_function(name))
            .collect::<Vec<_>>();
        Project {
            name: "test".to_string(),
            ast: ProjectAst {
                name: "test".to_string(),
                root_path: PathBuf::from("/test"),
                functions,
                structs: vec![],
            },
            root_dir: PathBuf::from("/test"),
            output_dir: PathBuf::from("/test/target/raster"),
            target_dir: PathBuf::from("/test/target/"),
        }
    }

    #[test]
    fn build_sorts_tiles_by_id_regardless_of_discovery_order() {
        // Two projects whose tile functions are discovered in opposite orders
        // (mimicking two filesystems' WalkDir orders) must produce byte-identical
        // CFS tile lists — the reproducibility property program identity needs.
        let forward = CfsBuilder::new(&project_with_tile_functions(&["alpha", "zeta", "mid"]))
            .build()
            .unwrap();
        let reversed = CfsBuilder::new(&project_with_tile_functions(&["zeta", "mid", "alpha"]))
            .build()
            .unwrap();

        let ids: Vec<&str> = forward.tiles.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "mid", "zeta"], "tiles sorted by id");
        assert_eq!(
            forward.tiles.iter().map(|t| &t.id).collect::<Vec<_>>(),
            reversed.tiles.iter().map(|t| &t.id).collect::<Vec<_>>(),
            "discovery order must not affect the canonical tile order",
        );
    }

    #[test]
    fn build_rejects_duplicate_tile_ids() {
        let err = CfsBuilder::new(&project_with_tile_functions(&["dup", "dup"]))
            .build()
            .unwrap_err();
        assert!(
            err.to_string().contains("duplicate tile id 'dup'"),
            "expected duplicate-id error, got: {err}"
        );
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
                chunk: None,
                output: None,
                state_is_output: false,
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
        assert!(
            seq_def.entry_arguments.is_empty(),
            "main declared no entry arguments"
        );
        assert_eq!(seq_def.items.len(), 1, "just the one tile item");
        assert!(matches!(seq_def.items[0], SequenceChildItem::Tile(_)));
    }
}

/// `S` out of a parameter type `wrapper<S>`, as the AST prints it
/// (`RecurOutput < CollectiveGreeting >`), including a path-qualified wrapper
/// (`raster :: RecurOutput < S >`).
fn generic_inner(ty: &str, wrapper: &str) -> Option<String> {
    let compact: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
    let start = compact.find(&format!("{wrapper}<"))?;
    let preceding = compact[..start].chars().last();
    if preceding.is_some_and(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let inner = &compact[start + wrapper.len() + 1..];
    let inner = inner.strip_suffix('>')?;
    Some(inner.to_string())
}
