use raster::prelude::*;

#[tile]
fn bad(_page: Bytes<4>) -> u64 {
    0
}

#[sequence]
fn main() {}
