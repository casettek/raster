//! The image ids of the guests FRAUD WINDOW verifies, compiled in by the
//! second pass of the guest build (`raster-prover/build.rs`). No image id is
//! read from the host, so a relying party pins only FRAUD WINDOW's own
//! (`docs/proposals/transition-split.md` §Image-id pinning).

const fn hex_value(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("an image id is lowercase hex"),
    }
}

const fn parse(hex: &str) -> [u8; 32] {
    let bytes = hex.as_bytes();
    assert!(bytes.len() == 64, "an image id is 64 hex characters");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = hex_value(bytes[2 * i]) * 16 + hex_value(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

pub const TRACE: [u8; 32] = parse(env!("RASTER_PART_ID_TRACE"));
pub const CONTROL_FLOW: [u8; 32] = parse(env!("RASTER_PART_ID_CONTROL_FLOW"));
pub const STORAGE: [u8; 32] = parse(env!("RASTER_PART_ID_STORAGE"));
pub const TRANSITION: [u8; 32] = parse(env!("RASTER_PART_ID_TRANSITION"));
pub const AUTHORIZATION: [u8; 32] = parse(env!("RASTER_PART_ID_AUTHORIZATION"));
