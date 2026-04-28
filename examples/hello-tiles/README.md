# Hello Tiles Example

A minimal example demonstrating tile, sequence, caller-owned external selectors, and committed external input usage.

## Structure

- `greet`: Creates a greeting from a string
- `personal_greet`: Selects `personal_data.name` at the call site
- `greet_address_line`: Selects `personal_data.address_lines[0]` at the call site
- `greet_sequence`: Chains tiles and nested sequences
- `bin/gen_input.rs`: Generates `input.json` and `input_manifest.json`

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
