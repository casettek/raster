//! Tile execution helper for native backend subprocess communication.
//!
//! This module provides helper functions that user projects can call from their
//! main() to handle tile execution requests from the raster CLI's native backend,
//! and to parse input arguments.

use raster_core::registry::find_tile_by_str;

use serde::de::DeserializeOwned;

use crate::utils::input;

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
    input::parse_main_input()
}
