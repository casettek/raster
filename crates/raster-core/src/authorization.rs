//! Shared types for manifest-backed external input authorization.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestedInputs {
    pub manifest_bytes: Vec<u8>,
    pub external_inputs_bytes: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationJournal {
    pub external_inputs_commitments: BTreeMap<String, Vec<u8>>,
    pub manifest_commitment: Vec<u8>,
}

