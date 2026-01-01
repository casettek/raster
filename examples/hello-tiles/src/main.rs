//! Hello Tiles Example
//!
//! This example demonstrates the Raster tile system:
//! - Defining tiles with the `#[tile]` macro
//! - Tile metadata (description, estimated_cycles)
//! - The tile registry and runtime discovery
//! - Executing tiles via the bincode ABI
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

use raster::prelude::*;

/// A simple tile that greets a user by name.
///
/// This tile takes a String input and returns a greeting.
#[tile]
fn greet(name: String) -> String {
    format!("Hello, {}!", name)
}

/// A tile that adds emphasis to a message.
///
/// This tile takes a String and returns it with exclamation marks.
#[tile]
fn exclaim(message: String) -> String {
    format!("{}!!!", message)
}

/// A tile that computes Fibonacci numbers.
///
/// This demonstrates a more computationally intensive tile.
#[tile]
fn fibonacci(n: u64) -> u64 {
    if n <= 1 {
        return n;
    }
    let mut a = 0u64;
    let mut b = 1u64;
    for _ in 2..=n {
        let c = a.wrapping_add(b);
        a = b;
        b = c;
    }
    b
}

#[sequence]
fn hello_sequence() {
    let greeting = greet("Raster".to_string());
    let excited = exclaim(greeting);
    println!("{}", excited);
}

fn main() {
    println!("=== Hello Tiles Example ===");
    println!();

    // ==========================================
    // Part 1: Tile Discovery via Registry
    // ==========================================
    println!("1. Tile Discovery");
    println!("   Registered tiles: {}", tile_count());
    println!();

    for tile in iter_tiles() {
        println!("   Tile: {} (id: {})", tile.metadata.name, tile.id_str());
        if let Some(desc) = tile.metadata.description {
            println!("     Description: {}", desc);
        }
        if let Some(cycles) = tile.metadata.estimated_cycles {
            println!("     Estimated cycles: {}", cycles);
        }
        if let Some(memory) = tile.metadata.max_memory {
            println!("     Max memory: {} bytes", memory);
        }
        println!();
    }

    // ==========================================
    // Part 2: Direct Tile Calls
    // ==========================================
    println!("2. Direct Tile Calls");
    println!("   greet(\"World\") = {}", greet("World".to_string()));
    println!("   exclaim(\"Hello\") = {}", exclaim("Hello".to_string()));
    println!("   fibonacci(10) = {}", fibonacci(10));
    println!();

    // ==========================================
    // Part 3: Registry-based Execution (Bincode ABI)
    // ==========================================
    println!("3. Registry-based Execution (Bincode ABI)");

    // Execute greet tile via registry
    if let Some(tile) = find_tile_by_str("greet") {
        let input = raster::core::bincode::serialize(&"zkVM".to_string()).unwrap();
        let output = tile.execute(&input).unwrap();
        let result: String = raster::core::bincode::deserialize(&output).unwrap();
        println!("   greet via registry: {}", result);
    }

    // Execute fibonacci tile via registry
    if let Some(tile) = find_tile_by_str("fibonacci") {
        let input = raster::core::bincode::serialize(&20u64).unwrap();
        let output = tile.execute(&input).unwrap();
        let result: u64 = raster::core::bincode::deserialize(&output).unwrap();
        println!("   fibonacci(20) via registry: {}", result);
    }
    println!();

    // ==========================================
    // Part 4: Information about CLI usage
    // ==========================================
    println!("4. CLI Usage");
    println!("   To build tiles for RISC0 zkVM:");
    println!("     cargo raster build --backend risc0");
    println!();
    println!("   To run a tile in estimate mode (no proof):");
    println!("     cargo raster run --tile greet --input '\"World\"'");
    println!();
    println!("   To run with proof generation:");
    println!("     cargo raster run --backend risc0 --tile greet --input '\"World\"' --prove");
    println!();
    println!("   To run with proof generation and verification:");
    println!("     cargo raster run --backend risc0 --tile greet --input '\"World\"' --prove --verify");
    println!();

    println!("=== Example Complete ===");
}
