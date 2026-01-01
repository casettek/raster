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
//! List all registered tiles:
//! ```sh
//! cargo raster list
//! ```

// Re-export tiles from the library
use hello_tiles::{greet, exclaim};

fn main() {
    let greeting = greet("Raster".to_string());
    let excited = exclaim(greeting);
    println!("{}", excited);
}
