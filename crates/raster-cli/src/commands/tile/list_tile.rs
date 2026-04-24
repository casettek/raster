use crate::commands::project_path;
use raster_compiler::tile::TileDiscovery;
use raster_compiler::Project;
use raster_core::Result;

/// List command: show all registered tiles.
pub fn list_tiles() -> Result<()> {
    let project = Project::new(project_path())?;
    let tile_explorer = TileDiscovery::new(&project);

    for tile in tile_explorer.tiles.iter() {
        println!("  {}", tile.function.signature);
        if let Some(description) = &tile.description {
            println!("    Description: {}", description);
        }
        if let Some(cycles) = &tile.estimated_cycles {
            println!("    Estimated cycles: {}", cycles);
        }
        if let Some(memory) = &tile.max_memory {
            println!("    Max memory: {} bytes", memory);
        }
        println!("    Source: {}", tile.source_file().display());
        println!();
    }

    Ok(())
}
