extern crate alloc;

use alloc::string::String;
use raster::List;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, raster::Selectable)]
pub struct Address {
    pub lines: List<String>,
    pub indexes: List<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, raster::Selectable)]
pub struct PersonalData {
    pub age: usize,
    pub name: String,
    pub addresses: List<Address>,
}

#[derive(Debug, Clone, Serialize, Deserialize, raster::Selectable)]
pub struct CollectiveGreeting {
    pub title: String,
    pub lines: List<String>,
}
