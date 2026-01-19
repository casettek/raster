use hello_tiles::{exclaim, greet};

/// The main sequence that greets and adds emphasis.
///
/// This sequence:
/// 1. Takes a name as input
/// 2. Generates a greeting with `greet`
/// 3. Adds emphasis with `exclaim`
///
/// Run with: `cargo run -- --input '"Raster"'`
/// Or: `cargo raster preview --input '"Raster"'`
#[raster::sequence]
fn greet_sequence(name: String) -> String {
    let greeting = greet(name);
    let e1 = exclaim(greeting);
    let e2 = exclaim(e1);
    exclaim(e2)
}

/// Entry point that runs the greet sequence natively.
///
/// The `name` parameter is parsed from `--input` CLI argument.
/// Run with: `cargo run -- --input '"YourName"'`
#[raster::main]
fn main(name: String) {
    let result = greet_sequence(name);
    println!("{}", result);
}
