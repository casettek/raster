use raster::prelude::*;

use hello_tiles::input::{Address, CollectiveGreeting, PersonalData};
use hello_tiles::*;

#[sequence]
fn personal_greet_seq(personal_data: PersonalData) -> Result<String> {
    let name = call!(maybe_echo_name, select!(String, personal_data.clone().name))?;

    let addresses = select!(Vec<Address>, personal_data.addresses);
    let address = select!(Address, addresses[1]);
    let address_line = select!(String, address.lines[0]);

    let greet_address_line_result = call!(greet_address_line, address_line);

    let result = call!(concat_messages, name, greet_address_line_result);

    println!("personal_greet_seq result ref: {:?}", result);

    Ok(result)
}

#[sequence]
fn greet_sequence(name: String, personal_data: PersonalData) -> String {
    call!(personal_greet, select!(String, personal_data.name));
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
/// This example binds its committed external inputs directly as declared
/// `main` parameters (see `docs/specs`), each resolved lazily from
/// `input.json`/`input_manifest.json` the first time it's actually
/// selected or used:
/// - `personal_data.name` is selected from a postcard-encoded struct using schema-driven DSL paths
/// - `personal_data_bin.addresses[0].lines[0]` is selected from the same struct loaded via mmap
/// - `personal_data_bin` is also used as a whole `PersonalData` object
/// - `seed` is used as a whole postcard-encoded value
///
/// Each argument must have a matching public commitment in `input_manifest.json`
/// (a selection-tree structural root for postcard-encoded arguments — see
/// `raster::postcard_structural_commitment`, used by `bin/gen_input.rs`).
/// Run with generated fixtures:
/// `cargo run --features gen-input --bin gen_input -- .` then
/// `cargo run -- --input input.json --input-manifest input_manifest.json`
///
#[sequence]
fn main(personal_data: PersonalData, personal_data_bin: PersonalData, seed: u64) {
    call_seq!(greet_sequence, "Rust".to_string(), personal_data.clone());

    let name = select!(String, personal_data.clone().name);
    let seed = select!(u64, seed);

    call!(personal_greet_with_seed, name, seed);

    let _personal_greet_seq_result =
        call_seq!(personal_greet_seq, personal_data).expect("wrong personal data");

    call!(
        greet_address_line,
        select!(String, personal_data_bin.clone().addresses[0].lines[0])
    );

    let selected_personal_data_bin = select!(PersonalData, personal_data_bin.clone());
    call!(personal_greet_from_object, selected_personal_data_bin);

    let draft = new!(CollectiveGreeting);
    let draft = call!(
        set_draft_greeting_title,
        "Draft-built greeting".to_string(),
        draft
    );
    let draft = call!(
        push_draft_greeting_line,
        "Hello from a Draft object".to_string(),
        draft
    );
    let draft = call!(
        push_draft_greeting_line,
        "This line was appended in a second tile".to_string(),
        draft
    );
    let draft_greeting = finalize(draft);
    println!("draft greeting: {:?}", draft_greeting);
    let draft_title = select!(String, draft_greeting.clone().title);
    let first_draft_line = select!(String, draft_greeting.lines[0]);
    call!(concat_messages, draft_title, first_draft_line);

    let address_lines = select!(Vec<String>, personal_data_bin.clone().addresses[0].lines);
    let recur_greeting = call_recur!(
        tile = build_recur_draft_greeting,
        input = address_lines.clone(),
        output = new!(CollectiveGreeting),
        args = ("Recur-built greeting".to_string(),)
    );
    println!("output-only recur greeting: {:?}", recur_greeting);
    let recur_title = select!(String, recur_greeting.clone().title);
    let recur_first_line = select!(String, recur_greeting.lines[0]);
    call!(concat_messages, recur_title, recur_first_line);

    let recur_line_stats = call_recur!(
        tile = compute_recur_max_line_len,
        input = address_lines.clone(),
        state = LineLengthStats { max_len: 0 },
        args = ()
    );
    println!("state-only recur stats: {:?}", recur_line_stats);
    let recur_max_line_len = select!(u64, recur_line_stats.max_len);
    call!(fibonacci, recur_max_line_len);

    let limited_recur_greeting = call_recur!(
        tile = build_limited_recur_greeting,
        input = address_lines,
        state = GreetingLimitState { seen: 0 },
        output = new!(CollectiveGreeting),
        args = ("State+output recur greeting".to_string(), 2)
    );
    println!("state+output recur greeting: {:?}", limited_recur_greeting);
    let limited_title = select!(String, limited_recur_greeting.clone().title);
    let limited_first_line = select!(String, limited_recur_greeting.lines[0]);
    call!(concat_messages, limited_title, limited_first_line);

    let name_2 = call_seq!(placeholder_sequence, "Placeholder".to_string());
    let result = call_seq!(greet_sequence, name_2, personal_data_bin);
    println!("main result: {:?}", result);
}
