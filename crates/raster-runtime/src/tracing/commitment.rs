use raster_core::trace::{FnInput, FnOutput};
use sha2::{Digest, Sha256};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Commitment(pub [u8; 32]);

impl Sha256Commitment {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl AsRef<[u8]> for Sha256Commitment {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<Sha256Commitment> for [u8; 32] {
    fn from(commitment: Sha256Commitment) -> [u8; 32] {
        commitment.0
    }
}

impl From<[u8; 32]> for Sha256Commitment {
    fn from(bytes: [u8; 32]) -> Self {
        Sha256Commitment(bytes)
    }
}

impl From<&[u8]> for Sha256Commitment {
    fn from(bytes: &[u8]) -> Self {
        Sha256Commitment(Sha256::digest(bytes).into())
    }
}

impl From<Sha256Commitment> for Vec<u8> {
    fn from(commitment: Sha256Commitment) -> Vec<u8> {
        commitment.0.to_vec()
    }
}

impl From<&FnInput> for Sha256Commitment {
    fn from(input: &FnInput) -> Self {
        Sha256Commitment::from(input.data())
    }
}

impl From<&FnOutput> for Sha256Commitment {
    fn from(output: &FnOutput) -> Self {
        Sha256Commitment::from(output.data())
    }
}
