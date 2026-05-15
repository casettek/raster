extern crate alloc;
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    raster::Selectable,
    raster::Merklized
)]
pub struct Address {
    pub lines: alloc::vec::Vec<String>,
    pub indexes: alloc::vec::Vec<u32>,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    raster::Selectable,
    raster::Merklized
)]
pub struct PersonalData {
    pub age: usize,
    pub name: String,
    pub addresses: alloc::vec::Vec<Address>,
}
