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
#[sequence]
fn greet_sequence(name: String) -> String {
    let greeting = greet(name);
    let e1 = exclaim(greeting);
    let e2 = exclaim(e1);
    exclaim(e2)
}

/// Entry point that runs the greet sequence natively.
fn main() {
    // Handle raster CLI tile execution requests (for native backend)
    if try_execute_tile_from_args() {
        return;
    }

    // Normal execution
    let result = greet_sequence("Raster".to_string());
    println!("{}", result);
}
