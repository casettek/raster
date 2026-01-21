use crate::{ProjectAst, ast::FunctionAstItem};

#[derive(Debug, Clone)]
pub struct Tile<'ast> {
    pub function: &'ast FunctionAstItem,
    pub estimated_cycles: Option<u64>,
    pub max_memory: Option<u64>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TileExplorer<'ast> {
    pub tiles: Vec<Tile<'ast>>,
}

impl<'ast> TileExplorer<'ast> {
    pub fn new(project_ast: &'ast ProjectAst) -> Self {
        let tiles = project_ast
            .functions
            .iter()
            .filter(|f| f.macros.iter().any(|m| m.name == "tile" || m.name == "raster::tile"))
            .map(|f| Self::extract_tile(f))
            .collect();

        Self { tiles }
    }

    fn extract_tile(func: &'ast FunctionAstItem) -> Tile<'ast> {
        let tile_macro = func
            .macros
            .iter()
            .find(|m| m.name == "tile" || m.name == "raster::tile");

        let estimated_cycles = tile_macro
            .and_then(|m| m.args.get("estimated_cycles"))
            .and_then(|v| v.parse().ok());

        let max_memory = tile_macro
            .and_then(|m| m.args.get("max_memory"))
            .and_then(|v| v.parse().ok());

        let description = tile_macro
            .and_then(|m| m.args.get("description").cloned());

        Tile {
            function: func,
            estimated_cycles,
            max_memory,
            description,
        }
    }

    /// Find a tile by name
    pub fn get(&self, name: &str) -> Option<&Tile<'ast>> {
        self.tiles.iter().find(|t| t.function.name == name)
    }

    /// Check if a function name is a tile
    pub fn contains(&self, name: &str) -> bool {
        self.tiles.iter().any(|t| t.function.name == name)
    }
}