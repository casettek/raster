use sha2::{Digest, Sha256};
use raster_core::trace::{FnInput, FnOutput};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Commitment(pub [u8; 32]);
  
impl Into<Vec<u8>> for Sha256Commitment {
  fn into(self) -> Vec<u8> {
      self.0.to_vec()
  }
}

impl Into<[u8; 32]> for Sha256Commitment {
  fn into(self) -> [u8; 32] {
      self.0
  }
}

impl From<&FnInput> for Sha256Commitment {
  fn from(input: &FnInput) -> Self {
      Sha256Commitment(Sha256::digest(input.data()).into())
  }
}

impl From<&FnOutput> for Sha256Commitment {
  fn from(output: &FnOutput) -> Self {
      Sha256Commitment(Sha256::digest(output.data()).into())
  }
}
