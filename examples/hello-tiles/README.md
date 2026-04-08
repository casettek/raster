# Hello Tiles Example

A minimal example demonstrating tile, sequence, and external input usage.

## Structure

- `greet`: Creates a greeting from a string
- `personal_greet`: Resolves `PersonalData` from an external postcard file
- `greet_sequence`: Chains tiles and nested sequences
- `bin/gen_input.rs`: Generates `personal_data.bin` and matching `input.json`

## Running

Generate the example input files:

```bash
cargo run --bin gen_input
```

Then run the example with the generated descriptor:

```bash
cargo run -- --input input.json
```

Or use the Raster CLI:

```bash
cargo raster preview --input input.json
```
