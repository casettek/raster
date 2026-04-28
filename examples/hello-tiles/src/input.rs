extern crate alloc;
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalData {
    pub age: usize,
    pub name: String,
    pub address_lines: alloc::vec::Vec<String>,
}
