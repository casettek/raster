extern crate alloc;

use alloc::string::String;
use core::result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Error {
    MissingName,
}

pub type Result<T> = result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize, raster::Selectable)]
pub struct Address {
    pub lines: alloc::vec::Vec<String>,
    pub indexes: alloc::vec::Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, raster::Selectable)]
pub struct PersonalData {
    pub age: usize,
    pub name: String,
    pub addresses: alloc::vec::Vec<Address>,
}
