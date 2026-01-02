use hello_tiles::{exclaim, greet};
use raster::prelude::*;

/// The main sequence that greets and adds emphasis.
///
/// This sequence:o0
/// 1. Takes a name as input
/// 2. Generates a greeting with `greet`
/// 3. Adds emphasis with `exclaim`
///
/// Run with: `cargo raster preview --input '"Raster"'`
#[sequence]
fn greet_sequence(name: String) -> String {
    let greeting = greet(name);
    exclaim(greeting)
}

/// Entry point that runs the greet sequence natively.
fn main() {
    let result = greet_sequence("Raster".to_string());
    println!("{}", result);
}
