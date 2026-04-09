use raster::prelude::*;

use hello_tiles::{current_wish, exclaim, greet, input::PersonalData, personal_greet, raster_wish};

/// The main sequence that greets and adds emphasis.
///
/// This sequence:
/// 1. Takes a name as input
/// 2. Generates a greeting with `greet`
/// 3. Adds emphasis with `exclaim`
/// 4. Invokes nested sequences for further processing
///
/// Run with: `cargo run -- --input '"Raster"'`
/// Or: `cargo raster preview --input '"Raster"'`
#[sequence]
fn greet_sequence(name: String) -> String {
    call!(personal_greet, external!("personal_data"));
    let greeting = call!(greet, name);
    let e1 = call!(exclaim, greeting);
    let e2 = call!(exclaim, e1);
    let wished = call_seq!(wish_sequence, e2);
    let exclaimed = call!(exclaim, wished);
    call_seq!(wish_sequence, exclaimed)
}

#[sequence]
fn wish_sequence(name: String) -> String {
    let raster_wished = call!(raster_wish, name);
    let wish = call!(current_wish, raster_wished);
    let wish_2 = call_seq!(placeholder_sequence, wish);
    call_seq!(placeholder_sequence, wish_2)
}

#[sequence]
fn placeholder_sequence(placeholder: String) -> String {
    call!(exclaim, placeholder)
}

/// Entry point that runs the greet sequence natively.
///
/// The `name` parameter is parsed from `--input` CLI argument.
/// Run with: `cargo run -- --input '"YourName"'`
#[sequence]
fn main(#[external(name = "personal_data")] personal_data: External<PersonalData>) {
    call_seq!(greet_sequence, "Rust".to_string());
    let name_2 = call_seq!(placeholder_sequence, personal_data.name);
    let _result = call_seq!(greet_sequence, name_2);
}
