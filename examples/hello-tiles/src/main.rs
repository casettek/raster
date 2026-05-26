use raster::prelude::*;

use hello_tiles::input::{Address, PersonalData};
use hello_tiles::{
    current_wish, exclaim, greet, greet_address_line, personal_greet, personal_greet_from_object,
    personal_greet_with_seed, raster_wish,
};

#[sequence]
fn personal_greet_seq(personal_data: PersonalData) -> Result<String> {
    let addresses = select!(Vec<Address>, personal_data.addresses);
    let address = select!(Address, addresses[1]);
    let address_line = select!(String, address.lines[0]);
    let greet_with_address_lint = call!(greet_address_line, address_line);

    Ok(greet_with_address_lint)
}

#[sequence]
fn greet_sequence(name: String) -> String {
    call!(
        personal_greet,
        select!(String, external!(PersonalData, "personal_data").name)
    );
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
/// - `personal_data.name` is selected from a postcard-encoded struct file using schema-driven DSL paths
/// - `personal_data_bin.addresses[0].lines[0]` is selected from a postcard-encoded struct file
/// - `personal_data_bin` is also selected as a whole `PersonalData` object
/// - `seed` is selected as a whole postcard-encoded value
///
/// Each input must have a matching public commitment in `input_manifest.json`.
/// Run with generated fixtures:
/// `cargo run -- --input input.json --input-manifest input_manifest.json`
///
#[sequence]
fn main() {
    call_seq!(greet_sequence, "Rust".to_string());

    let personal_data_binding = external!(PersonalData, "personal_data");
    let name = select!(String, personal_data_binding.clone().name);

    let seed_binding = external!(u64, "seed");
    let seed = select!(u64, seed_binding);

    call!(personal_greet_with_seed, name, seed);

    call_seq!(personal_greet_seq, personal_data_binding);

    call!(
        greet_address_line,
        select!(
            String,
            external!(PersonalData, "personal_data_bin").addresses[0].lines[0]
        )
    );

    let personal_data_bin = external!(PersonalData, "personal_data_bin");
    let selected_personal_data_bin = select!(PersonalData, personal_data_bin);
    call!(personal_greet_from_object, selected_personal_data_bin);

    let name_2 = call_seq!(placeholder_sequence, "Placeholder".to_string());
    let result = call_seq!(greet_sequence, name_2);
    debug!("main result: {}", result);
}
