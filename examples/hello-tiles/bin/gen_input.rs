use hello_tiles::input::PersonalData;
use raster::core::postcard;
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
    };

    let bytes = postcard::to_allocvec(&data)?;
    let hash = sha256_hex(&bytes);
    let bin_path = out_dir.join("personal_data.bin");
    let input_path = out_dir.join("input.json");

    fs::create_dir_all(&out_dir)?;
    fs::write(&bin_path, bytes)?;
    fs::write(
        &input_path,
        format!(
            concat!(
                "{{\n",
                "  \"personal_data\": {{\n",
                "    \"path\": \"personal_data.bin\",\n",
                "    \"data_hash\": \"{}\"\n",
                "  }}\n",
                "}}\n"
            ),
            hash
        ),
    )?;

    println!("Wrote {}", bin_path.display());
    println!("Wrote {}", input_path.display());

    Ok(())
}
