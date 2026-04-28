use hello_tiles::input::PersonalData;
use raster::core::postcard;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    let out_dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".to_string()));

    let data = PersonalData {
        age: 25,
        name: "John".to_string(),
        address_lines: vec!["221B Baker Street".to_string(), "Flat B".to_string()],
    };

    let personal_data_json = serde_json::to_value(&data)?;
    let json_bytes = serde_json::to_vec(&personal_data_json)?;
    let hash = sha256_hex(&json_bytes);
    let postcard_bytes = postcard::to_allocvec(&data)?;
    let postcard_hash = sha256_hex(&postcard_bytes);
    let bin_path = out_dir.join("personal_data.bin");
    let seed_bytes = serde_json::to_vec(&json!(123u64))?;
    let seed_hash = sha256_hex(&seed_bytes);
    let input_path = out_dir.join("input.json");
    let manifest_path = out_dir.join("input_manifest.json");

    fs::create_dir_all(&out_dir)?;
    fs::write(&bin_path, postcard_bytes)?;
    fs::write(
        &input_path,
        format!(
            concat!(
                "{{\n",
                "  \"personal_data\": {},\n",
                "  \"personal_data_bin\": {{ \"path\": \"personal_data.bin\" }},\n",
                "  \"seed\": 123\n",
                "}}\n"
            ),
            serde_json::to_string_pretty(&personal_data_json)?,
        ),
    )?;
    fs::write(
        &manifest_path,
        format!(
            concat!(
                "{{\n",
                "  \"personal_data\": {{ \"type\": \"sha256\", \"commitment\": \"{}\" }},\n",
                "  \"personal_data_bin\": {{ \"type\": \"sha256\", \"commitment\": \"{}\" }},\n",
                "  \"seed\": {{ \"type\": \"sha256\", \"commitment\": \"{}\" }}\n",
                "}}\n"
            ),
            hash, postcard_hash, seed_hash
        ),
    )?;

    println!("Wrote {}", bin_path.display());
    println!("Wrote {}", input_path.display());
    println!("Wrote {}", manifest_path.display());

    Ok(())
}
