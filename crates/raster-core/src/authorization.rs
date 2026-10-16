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
    /// Each value is the manifest's **lowercase hex text**, not the 32-byte
    /// digest it names: the authorization guest normalizes the manifest
    /// string and takes `String::into_bytes`, so a sha256 entry is 64 ASCII
    /// bytes. `chain_fraud` compares against it with `hex_lower`, which is
    /// what fixes the encoding.
    ///
    /// A consumer that needs the digest must decode first. Feeding these
    /// bytes to `struct_commitments_root` hashes the digest's *spelling* and
    /// yields a root that can never equal the entry object the runtime
    /// builds from raw commitments — see
    /// `checks::entrypoint::decode_authorized_commitment`.
    pub external_inputs_commitments: BTreeMap<String, Vec<u8>>,
    /// `sha256` of the raw input-manifest bytes — the document digest naming
    /// the authorized inputs. Renamed from `manifest_commitment`; paired with
    /// `output_manifest_commitment` on the output side. See
    /// `docs/proposals/program-identity.md`.
    pub input_manifest_commitment: Vec<u8>,
}
