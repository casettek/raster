# Hello Tiles Example

A minimal example demonstrating tile, sequence, and external input usage.

## Structure

- `greet`: Creates a greeting from a string
- `personal_greet`: Resolves `PersonalData` from an external postcard file
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
