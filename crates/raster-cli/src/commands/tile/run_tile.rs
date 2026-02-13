use raster_backend::ExecutionMode;
use raster_compiler::builder::Builder;
use raster_compiler::tile::TileDiscovery;
use raster_compiler::Project;
/// Run command: execute a tile with the specified backend.
use raster_core::Result;

use crate::utils::encode::{decode_output, encode_input};
use crate::{
    commands::{create_backend, project_path},
    BackendType,
};

pub fn run_tile(
    backend_type: BackendType,
    tile_id: &str,
    input: Option<&str>,
    prove: bool,
    verify: bool,
) -> Result<()> {
    // Determine execution mode
    let mode = match (prove, verify) {
        (_, true) => ExecutionMode::prove_and_verify(),
        (true, false) => ExecutionMode::prove(),
        (false, false) => ExecutionMode::Estimate,
    };

    let project = Project::new(project_path())?;

    let tile_discovery = TileDiscovery::new(&project);
    let tile = tile_discovery.get(tile_id).unwrap();

    let backend = create_backend(backend_type)?;

    let builder = Builder::new(&project, &backend);

    let tile_runner = builder.build_tile_runner(&tile)?;

    tile_runner.validate_input(input)?;
    let input_bytes = encode_input(input)?;
    let result = tile_runner.run(&input_bytes, mode)?;

    let output_display = decode_output(
        tile.function.output.as_deref().unwrap_or("()"),
        &result.output,
    );

    println!();
    println!("  Output: {}", output_display);
    println!("Execution complete!");
    if let Some(cycles) = result.cycles {
        println!("  Compute Cycles: {}", cycles);
    }
    // Show proof cycles in estimate mode to help users understand proving cost
    if let Some(proof_cycles) = result.proof_cycles {
        if result.receipt.is_none() {
            // Only show this hint in estimate mode
            println!(
                "  Proof cycles: {} (padded for STARK proving)",
                proof_cycles
            );
        }
    }
    if result.receipt.is_some() {
        println!(
            "  Receipt: generated ({} bytes)",
            result.receipt.as_ref().map(|r| r.len()).unwrap_or(0)
        );
    }
    if let Some(verified) = result.verified {
        println!("  Verified: {}", verified);
    }

    Ok(())
}
