# Hello Tiles Example

A minimal example demonstrating tile, sequence, schema-driven selection DSL, and file-backed committed externals.

The entrypoint uses `#[sequence] fn main()` and binds committed inputs explicitly with
`external!(...)` inside the body.

## Structure

- `greet`: Creates a greeting from a string
- `personal_greet`: Selects `personal_data.name` with `select!(external!(PersonalData, "personal_data").name)`
- `personal_greet_from_object`: Resolves a file-backed `External<PersonalData>`
- `greet_address_line`: Selects `personal_data_bin.addresses[0].lines[0]` from postcard-backed structured data
- `greet_sequence`: Chains tiles and nested sequences
- `bin/gen_input.rs`: Generates `personal_data.bin`, `input.json`, and `input_manifest.json`

## Running

Generate the example input files:

```bash
cargo run --bin gen_input --features gen-input
```

Then run the example with the generated private input and public manifest:

```bash
cargo run --bin hello-tiles -- --input input.json --input-manifest input_manifest.json
```

Or use the Raster CLI:

```bash
cargo raster run --input input.json --input-manifest input_manifest.json
```
