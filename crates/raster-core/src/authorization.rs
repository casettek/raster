//! Shared types for manifest-backed external input authorization.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizedExternalInput {
    pub commitment: Vec<u8>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizedExternalInputs {
    pub entries: BTreeMap<String, AuthorizedExternalInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestedInputs {
    pub manifest_bytes: Vec<u8>,
    pub external_inputs_bytes: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationJournal {
    pub authorized_external_inputs: AuthorizedExternalInputs,
    pub manifest_commitment: Vec<u8>,
}

