# Hello Tiles Example

A minimal example demonstrating basic tile and sequence usage.

## Structure

- `greet`: A simple tile that creates a greeting
- `exclaim`: A tile that adds emphasis to a message
- `hello_sequence`: A sequence that chains the two tiles

## Running

```bash
cargo raster build
cargo raster run
```

````rust
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
````
