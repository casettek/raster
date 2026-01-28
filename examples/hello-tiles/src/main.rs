use hello_tiles::{current_wish, exclaim, greet, raster_wish};
use std::fs::File;
use std::io::BufWriter;

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
    exclaim(wish_sequence(e2))
}

#[raster::sequence]
fn wish_sequence(name: String) -> String {
    current_wish(raster_wish(name))
}
/// Entry point that runs the greet sequence natively.
///
/// The `name` parameter is parsed from `--input` CLI argument.
/// Run with: `cargo run -- --input '"YourName"'`
#[raster::main]
fn main(name: String) {
    // let file = File::create("traces.json").expect("Failed to create trace file");
    
    // // Optional: wrap in BufWriter for better performance
    // let writer = BufWriter::new(file);
    
    // // Initialize the tracer with the file subscriber
    // raster::init_with(raster::JsonSubscriber::new(writer));

    let result = greet_sequence(name);
    println!("{}", result);
}
