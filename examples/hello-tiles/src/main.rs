use hello_tiles::{count_to, exclaim, greet};
use raster::prelude::*;

/// The main sequence that greets and adds emphasis, with recursive counting.
///
/// This sequence:
/// 1. Takes a name as input
/// 2. Generates a greeting with `greet`
/// 3. Adds emphasis with `exclaim`
/// 4. Recursively counts to 5 using `count_to!`
///
/// The `!` suffix on `count_to!` invokes the recursive tile, which will
/// execute repeatedly until its first output (done) returns true.
///
/// Run with: `cargo raster preview --input '"Raster"'`
#[sequence]
fn greet_sequence(name: String) -> (String, bool, u64, u64) {
    let greeting = greet(name);
    let emphasized = exclaim(greeting);
    let count_result = count_to!(0, 5);
    (emphasized, count_result.0, count_result.1, count_result.2)
}

/// Entry point that runs the greet sequence natively.
fn main() {
    let (message, done, current, goal) = greet_sequence("Raster".to_string());
    println!("Message: {}", message);
    println!("Count complete: {} (reached {} of {})", done, current, goal);
}
