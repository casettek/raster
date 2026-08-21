//! Generate phase 1's committed inputs.
//!
//! Writes the raster-encoded `measurements` and `threshold` artifacts plus this
//! stage's `input.json` / `input_manifest.json`, and prints the two structural
//! commitments so they can be pasted into the chain manifest's `external`
//! bindings.
//!
//! Run: `cargo run --features gen-input --bin gen_input -- .`

use chain_stage_normalize::Measurements;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let out_dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".to_string()));
    fs::create_dir_all(&out_dir)?;

    let measurements = Measurements {
        label: "sensor-A".to_string(),
        // A mix of below- and above-threshold readings.
        samples: vec![12, 47, 5, 88, 30, 61, 9, 74, 25, 53].into(),
    };
    let threshold: u64 = 30;

    let measurements_commitment = raster::write_raster_files(
        &measurements,
        &out_dir.join("measurements.rastered"),
        &out_dir.join("measurements.rindex"),
    )?;
    let threshold_commitment = raster::write_raster_files(
        &threshold,
        &out_dir.join("threshold.rastered"),
        &out_dir.join("threshold.rindex"),
    )?;

    fs::write(
        out_dir.join("input.json"),
        concat!(
            "{\n",
            "  \"readings\": { \"path\": \"measurements.rastered\", \"index_path\": \"measurements.rindex\", \"load_preference\": \"read\" },\n",
            "  \"threshold\": { \"path\": \"threshold.rastered\", \"index_path\": \"threshold.rindex\", \"load_preference\": \"read\" }\n",
            "}\n"
        ),
    )?;
    fs::write(
        out_dir.join("input_manifest.json"),
        format!(
            concat!(
                "{{\n",
                "  \"readings\": {{ \"type\": \"sha256\", \"encoding\": \"raster\", \"commitment\": \"{}\" }},\n",
                "  \"threshold\": {{ \"type\": \"sha256\", \"encoding\": \"raster\", \"commitment\": \"{}\" }}\n",
                "}}\n"
            ),
            measurements_commitment, threshold_commitment
        ),
    )?;

    println!("wrote measurements.rastered / .rindex  commitment = {measurements_commitment}");
    println!("wrote threshold.rastered / .rindex     commitment = {threshold_commitment}");
    println!();
    println!("paste into Raster.toml → [[chain.stage]] \"normalize\":");
    println!("  inputs.readings  external.commitment = {measurements_commitment}");
    println!("  inputs.threshold external.commitment = {threshold_commitment}");

    Ok(())
}
