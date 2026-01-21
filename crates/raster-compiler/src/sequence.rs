use raster_backend::Backend;

use crate::{
    ProjectAst, ast::FunctionAstItem, tile::{self, Tile, TileExplorer}
};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct Sequence<'ast> {
    pub function: &'ast FunctionAstItem,
    pub steps: Vec<SequenceStep<'ast>>,
    pub description: Option<String>,
}

impl<'ast> Sequence<'ast> {
    pub fn iter(&'ast self) -> impl Iterator<Item = &'ast SequenceStep<'ast>> {
        self.steps.iter()
    }
}

#[derive(Debug, Clone)]
pub enum SequenceStep<'ast> {
    Tile(&'ast Tile<'ast>), // Reference to tile
    Sequence(String),       // Sequence name (resolved later if needed)
}


#[derive(Debug, Clone)]
pub struct SequenceExplorer<'ast> {
    pub sequences: Vec<Sequence<'ast>>,
}

impl<'ast> SequenceExplorer<'ast> {
    pub fn new(project_ast: &'ast ProjectAst, tile_explorer: &'ast TileExplorer<'ast>) -> Self {
        let sequence_names: HashSet<String> = project_ast
            .functions
            .iter()
            .filter(|f| {
                f.macros
                    .iter()
                    .any(|m| m.name == "sequence" || m.name == "raster::sequence")
            })
            .map(|f| f.name.clone())
            .collect();

        let sequences = project_ast
            .functions
            .iter()
            .filter(|f| sequence_names.contains(&f.name))
            .map(|f| Self::extract_sequence(f, tile_explorer, &sequence_names))
            .collect();

        Self { sequences }
    }

    fn extract_sequence(
        func: &'ast FunctionAstItem,
        tile_explorer: &'ast TileExplorer<'ast>,
        sequence_names: &HashSet<String>,
    ) -> Sequence<'ast> {
        let description = func
            .macros
            .iter()
            .find(|m| m.name == "sequence" || m.name == "raster::sequence")
            .and_then(|m| m.args.get("description").cloned());

        let steps: Vec<SequenceStep<'ast>> = func
            .calls
            .iter()
            .filter_map(|call| {
                // Try to find tile by name
                if let Some(tile) = tile_explorer.get(call) {
                    Some(SequenceStep::Tile(tile))
                } else if sequence_names.contains(call) {
                    Some(SequenceStep::Sequence(call.clone()))
                } else {
                    None
                }
            })
            .collect();

        Sequence {
            function: func,
            steps,
            description,
        }
    }
}
