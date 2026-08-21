use raster::prelude::*;

#[tile]
fn bad(_bytes: Vec<u8>) -> u64 {
    0
}

#[sequence]
fn main() {}
