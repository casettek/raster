use std::path::PathBuf;

use crate::ast::FunctionAstItem;
use crate::Project;

use raster_core::tile::{TileId, TileMetadata};

#[derive(Debug, Clone)]
pub struct TileResult {
    pub output: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Tile<'ast> {
    pub function: &'ast FunctionAstItem,
    /// Tile type (e.g., "tile", "recur").
    pub tile_type: String,

    pub estimated_cycles: Option<u64>,
    pub max_memory: Option<u64>,
    pub description: Option<String>,
}

impl<'ast> Tile<'ast> {
    pub fn id(&self) -> &str {
        &self.function.name
    }

    pub fn source_file(&self) -> PathBuf {
        self.function.path.clone()
    }

    /// Convert this Tile to a TileMetadata with source_file populated.
    ///
    /// This is used when passing tile information to backends for compilation.
    /// The source_file allows backends (like RISC0) to compute hashes for caching.
    pub fn to_metadata(&self) -> TileMetadata {
        TileMetadata {
            id: TileId::new(self.id()),
            name: self.function.name.clone(),
            description: self.description.clone(),
            estimated_cycles: self.estimated_cycles,
            max_memory: self.max_memory,
        }
    }

    // TODO: move content hashing to the artifact store
    pub fn to_content_hash(&self) -> Option<String> {
        let mut file = std::fs::File::open(self.source_file()).ok()?;
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut contents).ok()?;

        // Simple hash: use the first 16 bytes of a basic checksum
        // This is fast and good enough for cache invalidation
        let mut hash: u64 = 0;
        for (i, byte) in contents.iter().enumerate() {
            hash = hash.wrapping_add((*byte as u64).wrapping_mul((i as u64).wrapping_add(1)));
            hash = hash.rotate_left(7);
        }
        let len_hash = contents.len() as u64;
        Some(format!("{:016x}{:016x}", hash, len_hash))
    }
}

#[derive(Debug, Clone)]
pub struct TileDiscovery<'ast> {
    pub project: &'ast Project,
    pub tiles: Vec<Tile<'ast>>,
}

impl<'ast> TileDiscovery<'ast> {
    pub fn new(project: &'ast Project) -> Self {
        let project_ast = &project.ast;
        let tiles = project_ast
            .functions
            .iter()
            .filter(|f| {
                f.macros
                    .iter()
                    .any(|m| m.name == "tile" || m.name == "raster::tile")
            })
            .map(Self::extract_tile)
            .collect();

        Self { project, tiles }
    }

    fn extract_tile(func: &'ast FunctionAstItem) -> Tile<'ast> {
        let tile_macro = func
            .macros
            .iter()
            .find(|m| m.name == "tile" || m.name == "raster::tile");

        let tile_type = tile_macro
            .and_then(|m| m.args.get("kind").cloned())
            .unwrap_or_else(|| "iter".to_string());

        let estimated_cycles = tile_macro
            .and_then(|m| m.args.get("estimated_cycles"))
            .and_then(|v| v.parse().ok());

        let max_memory = tile_macro
            .and_then(|m| m.args.get("max_memory"))
            .and_then(|v| v.parse().ok());

        let description = tile_macro.and_then(|m| m.args.get("description").cloned());

        Tile {
            function: func,
            tile_type,
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
