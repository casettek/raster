//! Shared types for manifest-backed external input authorization.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestedInputs {
    pub manifest_bytes: Vec<u8>,
}

/// Journal committed by the authorization guest: the full set of named
/// input commitments the public manifest declares, plus a commitment to
/// the manifest bytes themselves. Consumers (`checks::entrypoint` in the
/// transition guest) look up exactly the names the CFS declares as `main`
/// entry arguments; extra manifest entries are inert.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationJournal {
    pub external_inputs_commitments: BTreeMap<String, Vec<u8>>,
    pub manifest_commitment: Vec<u8>,
}
