use raster::prelude::*;

use hello_tiles::{count_to, current_wish, exclaim, greet, raster_wish};

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
/// Run with: `cargo run -- --input '"Raster"'`
/// Or: `cargo raster preview --input '"Raster"'`
#[sequence]
fn greet_sequence(name: String) -> String {
    let greeting = greet(name);
    let e1 = exclaim(greeting);
    let e2 = exclaim(e1);
    wish_sequence(exclaim(wish_sequence(e2)))
}

#[sequence]
fn wish_sequence(name: String) -> String {
    let wish = current_wish(raster_wish(name));
    let wish_2 = placeholder_sequence(wish);
    placeholder_sequence(wish_2)
}

#[sequence]
fn placeholder_sequence(placeholder: String) -> String {
    exclaim(placeholder)
}

/// Entry point that runs the greet sequence natively.
///
/// The `name` parameter is parsed from `--input` CLI argument.
/// Run with: `cargo run -- --input '"YourName"'`
#[sequence]
fn main(name: String) {
    greet_sequence("Rust".to_string());
    let name_2 = placeholder_sequence(name);
    let result = greet_sequence(name_2);
}
