//! Tile execution helper for native backend subprocess communication.
//!
//! This module provides helper functions that user projects can call from their
//! main() to handle tile execution requests from the raster CLI's native backend,
//! and to parse input arguments.

use crate::core::ipc;
use crate::core::registry::find_tile_by_str;
use serde::de::DeserializeOwned;

/// Check for --raster-exec arguments and execute the specified tile if present.
///
/// This function is used by the native backend's subprocess execution model.
///
/// # Preferred Usage
///
/// Use the `#[raster::main]` attribute macro, which handles this automatically:
///
/// ```rust,ignore
/// #[raster::main]
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
/// The function looks for: `--raster-exec <tile_id> --input <base64_input>`
///
/// On success, it prints: `RASTER_OUTPUT:<base64_output>`
/// On error, it prints an error message and exits with code 1.
#[cfg(feature = "std")]
pub fn try_execute_tile_from_args() -> bool {
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

    let input_b64 = match args.get(input_pos + 1) {
        Some(input) => input,
        None => {
            std::eprintln!("Error: --input requires a base64-encoded value");
            std::process::exit(1);
        }
    };

    // Decode input from base64
    use base64::Engine;
    let input = match base64::engine::general_purpose::STANDARD.decode(input_b64) {
        Ok(data) => data,
        Err(e) => {
            std::eprintln!("Error: Failed to decode input: {}", e);
            std::process::exit(1);
        }
    };

    // Find the tile in the registry
    let tile = match find_tile_by_str(tile_id) {
        Some(t) => t,
        None => {
            std::eprintln!("Error: Tile '{}' not found in registry", tile_id);
            std::process::exit(1);
        }
    };

    // Execute the tile
    let output = match tile.execute(&input) {
        Ok(out) => out,
        Err(e) => {
            std::eprintln!("Error: Tile execution failed: {}", e);
            std::process::exit(1);
        }
    };

    // Encode output as base64 and print with marker
    let output_b64 = base64::engine::general_purpose::STANDARD.encode(&output);
    ipc::emit_output(&output_b64);

    true
}

/// Parse the `--input` argument from command line and deserialize it.
///
/// This function is used by the `#[raster::main]` macro to parse input arguments
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

