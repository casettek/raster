//! Tile execution helper for native backend subprocess communication.
//!
//! This module provides helper functions that user projects can call from their
//! main() to handle tile execution requests from the raster CLI's native backend,
//! and to parse input arguments.

use raster_core::registry::find_tile_by_str;

use serde::de::DeserializeOwned;

/// Check for --raster-exec arguments and execute the specified tile if present.
///
/// This function is used by the native backend's subprocess execution model.
///
/// # Preferred Usage
///
/// Use `#[raster::sequence] fn main(...)`, which handles this automatically:
///
/// ```rust,ignore
/// #[raster::sequence]
/// fn main() {
///     // Your normal main logic...
/// }
/// ```
///
/// # Manual Usage
///
/// If you need more control, you can call this function directly:
///
/// ```rust,ignore
/// fn main() {
///     // Handle raster CLI tile execution requests
///     if raster::try_execute_tile_from_args() {
///         return;
///     }
///
///     // Normal main logic...
/// }
/// ```
///
/// # Returns
///
/// Returns `true` if a tile was executed (caller should exit immediately).
/// Returns `false` if no --raster-exec argument was found (normal execution).
///
/// # Protocol
///
///
/// On error, it prints an error message and exits with code 1.
#[cfg(feature = "std")]
pub fn try_execute_tile_from_args() -> bool {
    use raster_core::postcard;

    let args: std::vec::Vec<std::string::String> = std::env::args().collect();

    // Look for --raster-exec argument
    let exec_pos = match args.iter().position(|a| a == "--raster-exec") {
        Some(pos) => pos,
        None => return false,
    };

    // Get tile ID (next argument after --raster-exec)
    let tile_id = match args.get(exec_pos + 1) {
        Some(id) => id,
        None => {
            std::eprintln!("Error: --raster-exec requires a tile ID argument");
            std::process::exit(1);
        }
    };

    // Get input (look for --input argument)
    let input_pos = match args.iter().position(|a| a == "--input") {
        Some(pos) => pos,
        None => {
            std::eprintln!("Error: --raster-exec requires --input argument");
            std::process::exit(1);
        }
    };

    let input = match args.get(input_pos + 1) {
        Some(input) => input,
        None => {
            std::eprintln!("Error: --input requires a base64-encoded value");
            std::process::exit(1);
        }
    };

    let input_bytes = postcard::to_allocvec(input).expect("Failed to serialize input to bytes");

    // Find the tile in the registry
    let tile = match find_tile_by_str(tile_id) {
        Some(t) => t,
        None => {
            std::eprintln!("Error: Tile '{}' not found in registry", tile_id);
            std::process::exit(1);
        }
    };

    // Execute the tile
    let output = match tile.execute(&input_bytes) {
        Ok(out) => out,
        Err(e) => {
            std::eprintln!("Error: Tile execution failed: {}", e);
            std::process::exit(1);
        }
    };

    true
}

/// Parse the `--input` argument from command line and deserialize it.
///
/// This function is used by the `#[raster::sequence] fn main(...)` entry point to parse input arguments
/// passed via the command line. The input should be a JSON value.
///
/// # Example
///
/// ```bash
/// # For a String input:
/// cargo run -- --input '"Hello"'
///
/// # For a number:
/// cargo run -- --input '42'
///
/// # For a tuple (multiple parameters):
/// cargo run -- --input '["Hello", 42]'
/// ```
///
/// # Returns
///
/// Returns `Some(T)` if the `--input` argument was found and successfully deserialized.
/// Returns `None` if no `--input` argument was found or deserialization failed.
pub fn parse_main_input<T: DeserializeOwned>() -> Option<T> {
    let args: std::vec::Vec<std::string::String> = std::env::args().collect();

    // Look for --input argument
    let input_pos = args.iter().position(|a| a == "--input")?;
    let input_json = args.get(input_pos + 1)?;

    // Parse JSON directly into the target type
    serde_json::from_str(input_json).ok()
}
