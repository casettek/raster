use hello_tiles::input::{Address, PersonalData};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let out_dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".to_string()));
    fs::create_dir_all(&out_dir)?;

    let data = PersonalData {
        age: 25,
        name: "John".to_string(),
        addresses: vec![
            Address {
                lines: vec!["221B Baker Street".to_string(), "Flat B".to_string()],
                indexes: vec![7, 42],
            },
            Address {
                lines: vec!["Main Plaza".to_string()],
                indexes: vec![3],
            },
        ],
    };
    let seed: u64 = 123;

    // `main`'s entry arguments here are selected into (fields, whole
    // objects) both in-process and by the commit/audit pipeline's
    // cross-process fraud-proof recorder — raster encoding is required for
    // the latter, since its `.rindex` is self-describing (postcard entry
    // arguments only support in-process selection; see
    // `raster_runtime::tracing::recorder`).
    let personal_data_commitment = raster::write_raster_files(
        &data,
        &out_dir.join("personal_data.rastered"),
        &out_dir.join("personal_data.rindex"),
    )?;
    let seed_commitment = raster::write_raster_files(
        &seed,
        &out_dir.join("seed.rastered"),
        &out_dir.join("seed.rindex"),
    )?;

    let input_path = out_dir.join("input.json");
    let manifest_path = out_dir.join("input_manifest.json");

    fs::write(
        &input_path,
        concat!(
            "{\n",
            "  \"personal_data\": { \"path\": \"personal_data.rastered\", \"index_path\": \"personal_data.rindex\", \"load_preference\": \"read\" },\n",
            "  \"personal_data_bin\": { \"path\": \"personal_data.rastered\", \"index_path\": \"personal_data.rindex\", \"load_preference\": \"mmap\" },\n",
            "  \"seed\": { \"path\": \"seed.rastered\", \"index_path\": \"seed.rindex\", \"load_preference\": \"read\" }\n",
            "}\n"
        ),
    )?;
    fs::write(
        &manifest_path,
        format!(
            concat!(
                "{{\n",
                "  \"personal_data\": {{ \"type\": \"sha256\", \"encoding\": \"raster\", \"commitment\": \"{}\" }},\n",
                "  \"personal_data_bin\": {{ \"type\": \"sha256\", \"encoding\": \"raster\", \"commitment\": \"{}\" }},\n",
                "  \"seed\": {{ \"type\": \"sha256\", \"encoding\": \"raster\", \"commitment\": \"{}\" }}\n",
                "}}\n"
            ),
            personal_data_commitment, personal_data_commitment, seed_commitment
        ),
    )?;

    println!("Wrote {}", out_dir.join("personal_data.rastered").display());
    println!("Wrote {}", out_dir.join("seed.rastered").display());
    println!("Wrote {}", input_path.display());
    println!("Wrote {}", manifest_path.display());

    Ok(())
}
