//! Hello Tiles Example Binary
//!
//! ## CLI Usage
//!
//! Build tiles (native):
//! ```sh
//! cargo raster build
//! ```
//!
//! Build tiles (RISC0):
//! ```sh
//! cargo raster build --backend risc0
//! ```
//!
//! Run a tile (estimate mode - default):
//! ```sh
//! cargo raster run --tile greet --input '"World"'
//! ```
//!
//! Run a tile (prove mode):
//! ```sh
//! cargo raster run --backend risc0 --tile greet --input '"World"' --prove
//! ```
//!
//! Run a tile (prove and verify):
//! ```sh
//! cargo raster run --backend risc0 --tile greet --input '"World"' --prove --verify
//! ```
//!
//! Preview sequence with cycle counts:
//! ```sh
//! cargo raster preview --input '"Raster"'
//! ```
//!
//! List all registered tiles:
//! ```sh
//! cargo raster list
//! ```

use hello_tiles::{exclaim, greet};
use raster::prelude::*;

/// The main sequence that greets and adds emphasis.
///
/// This sequence:
/// 1. Takes a name as input
/// 2. Generates a greeting with `greet`
/// 3. Adds emphasis with `exclaim`
///
/// Run with: `cargo raster preview --input '"Raster"'`
#[sequence(description = "Greet and exclaim sequence")]
fn greet_sequence(name: String) -> String {
    let greeting = greet(name);
    exclaim(greeting)
}

/// Entry point that runs the greet sequence natively.
fn main() {
    let result = greet_sequence("Raster".to_string());
    println!("{}", result);
}
