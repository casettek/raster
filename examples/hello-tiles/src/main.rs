use raster::prelude::*;

use hello_tiles::{
    current_wish, exclaim, greet, greet_address_line, personal_greet,
    personal_greet_from_object, personal_greet_with_seed, raster_wish,
};

#[sequence]
fn greet_sequence(name: String) -> String {
    call!(personal_greet, external!("personal_data", select!("name")));
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
/// This example resolves committed external inputs from `input.json`:
/// - `personal_data.name` and `personal_data.address_lines[0]` are selected from inline JSON
/// - `personal_data_bin` is loaded from a binary postcard file into `External<PersonalData>`
/// - `seed` is provided inline in the JSON document
///
/// Each input must have a matching public commitment in `input_manifest.json`.
/// Run with generated fixtures:
/// `cargo run -- --input input.json --input-manifest input_manifest.json`
#[sequence]
fn main() {
    call_seq!(greet_sequence, "Rust".to_string());
    call!(
        personal_greet_with_seed,
        external!("personal_data", select!("name")),
        external!("seed")
    );
    call!(
        greet_address_line,
        external!("personal_data", select!("address_lines", 0))
    );
    call!(personal_greet_from_object, external!("personal_data_bin"));
    let name_2 = call_seq!(placeholder_sequence, "Placeholder".to_string());
    let result = call_seq!(greet_sequence, name_2);
    debug!("main result: {}", result);
}
