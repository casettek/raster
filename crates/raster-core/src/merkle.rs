//! Constants of the bridgetree Merkle trees behind trace and storage
//! commitments, shared by the host prover and the transition guest.
//!
//! Both sides hash an inner node as `sha256(level ‖ left ‖ right)` (`combine`
//! on `Bytes` in `raster-prover`'s `trace.rs` and in the transition guest's
//! `merkle_tree.rs`). [`EMPTY_ROOTS`] depends only on that rule and on
//! [`EMPTY_LEAF`]. Each side pins it to its own `combine` with a test, so the
//! two can never silently disagree.

/// Size of a node hash (SHA-256).
pub const HASH_SIZE: usize = 32;

/// Decode 64 hex digits at compile time. A local `const fn` rather than the
/// `hex-literal` crate: every tile guest links raster-core, and a new
/// dependency here changes raster-core's crate metadata, and with it every
/// tile's image id.
const fn hex(digits: &str) -> [u8; HASH_SIZE] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("expected a lowercase hex digit"),
        }
    }
    let digits = digits.as_bytes();
    assert!(digits.len() == 2 * HASH_SIZE, "expected 64 hex digits");
    let mut out = [0u8; HASH_SIZE];
    let mut i = 0;
    while i < HASH_SIZE {
        out[i] = (nibble(digits[2 * i]) << 4) | nibble(digits[2 * i + 1]);
        i += 1;
    }
    out
}

/// A 32-byte hash inside a protocol type: a merkle node, root or path element.
///
/// `serde_bytes::ByteArray` serializes as one byte string instead of a
/// sequence of 32 `u8`. postcard writes both the same way (length, then the
/// raw bytes), so commitments over postcard encodings are unchanged; decoding
/// becomes one copy instead of 32 element visits. risc0's serializer packs the
/// bytes into 8 words instead of spending one word per byte. It derefs to
/// `[u8; 32]` and converts from it with `.into()`, so code outside the protocol
/// types never names `serde_bytes`.
pub type NodeHash = serde_bytes::ByteArray<HASH_SIZE>;

/// A [`NodeHash`] from a hash held as bytes. Panics unless it is 32 bytes long:
/// every caller passes a SHA-256 output.
pub fn to_node_hash(bytes: &[u8]) -> NodeHash {
    let array: [u8; HASH_SIZE] = bytes.try_into().expect("a hash is 32 bytes");
    array.into()
}

/// Depth of the trace and storage trees: the level a frontier's root is folded
/// to.
pub const TRACE_TREE_DEPTH: u8 = 32;

/// The empty leaf. It also seeds every trace and storage tree as its first
/// leaf.
///
/// This is a fixed constant, not the hash of any recorded preimage (in
/// particular, not `sha256("empty")`). Changing it changes every commitment.
pub const EMPTY_LEAF: [u8; HASH_SIZE] =
    hex("6d97a6c02676a41a9636c6cd4e5d2d47d14d27a35d18e608115fd93cd42e6b3a");

/// Roots of empty subtrees for levels `0..=TRACE_TREE_DEPTH`:
/// `EMPTY_ROOTS[0] = EMPTY_LEAF` and
/// `EMPTY_ROOTS[l + 1] = sha256(l ‖ EMPTY_ROOTS[l] ‖ EMPTY_ROOTS[l])`.
///
/// Compiled in rather than memoized at runtime. Each transition step is a
/// fresh guest execution, so a runtime memo is rebuilt on every step, and
/// building it cost ~0.4M proven cycles per step.
pub const EMPTY_ROOTS: [[u8; HASH_SIZE]; TRACE_TREE_DEPTH as usize + 1] = [
    EMPTY_LEAF, // 0
    hex("abb2ffa35a31a148f8e596707ec471b4e66f82ee87b6482f8a450f7e2a2355b0"), // 1
    hex("13683036bbdbef76d6d5887cba0ae151037dd29508062d537c416491573568c2"), // 2
    hex("264a176f64a4db9e4a499e3f830f8b98e901317403862d6484707d1c517bb45c"), // 3
    hex("52afe2d458bef7aa22130d2a2cd3d363d20c13fd5606549cf32e545bf41462eb"), // 4
    hex("79c6e23a1da278432a60f809160c15c25e1311b548be007620e38d3581907a65"), // 5
    hex("33bf7a7fb3fda55a8ef0ba3aece5f9cab1a5f8b610b088a3a41febc61e4c0cc7"), // 6
    hex("85c83ea8248ff60630af0d61dee2f657098badb6b48d14cecfac1cf4c2404901"), // 7
    hex("5c16df777ce7762af369e7e518c737ec6a6ac6c62724e5adbcf67e01d2ba7d9b"), // 8
    hex("e865ace87d21a8455c6a732df92d1830831f3e45c70bcc5d28c5861b0db4f18e"), // 9
    hex("a54e710f78fe54be3bde100cd5b9c3634337b43347b632e6c92fa21b35e66a53"), // 10
    hex("533245b2a10aac83376e3f14bb6da35011cceea029c6a4276a374c52c04bc27a"), // 11
    hex("07e2303b55300c0e66de81d0ebb50c6df473d78f0d7d3881192f1a5815bcde2b"), // 12
    hex("f921ee70af99b995bfc5f55cf8c7568aea2821415efc31fd6a39521a8376d60e"), // 13
    hex("464dd1a3d705dd0d41449ee80b25dd095da429b7ea768968da725ec56557410b"), // 14
    hex("262de9181bada3b69ad23b8fd5948b86e7ad6fc8f89b049748c3cf90b10b665b"), // 15
    hex("48793c52290c26bf4688660eba192881cc2cc34e5a255c37f4e2b293c54c4550"), // 16
    hex("1d9a5562cf20ba9480641c001f7ac704c4325c5fc7289e800df85e849123ddc2"), // 17
    hex("0e1a61ca29a7a3142adb951c8b344952c6e9e5137a900eb9822097f736d6afa6"), // 18
    hex("6e7b50409b24f35e3b8aa4f99cbf430735541e78c655105186f143fc91ce95cb"), // 19
    hex("bcd7e9b4825a0acc8c8622f78e5540f6aa17ebb2b4ea73fadd8f1392b56fd43a"), // 20
    hex("c8441f1c54283f205f27cc67fd17961a8ca07aa49a41fb0de46e360cd17e7391"), // 21
    hex("ee4afcabb7f3974766537e254f695ba079e0a73235c2ec70ff8d2651b134a990"), // 22
    hex("9bd5310168a2e3acbacbef3b58833f7fedf7503a03972ec4b0c4243974e4d0fc"), // 23
    hex("25efc95fc4ca4ab202614090132978dfd2e0905fc2d96d877888f55826141361"), // 24
    hex("47dc2e21212290ea8ab2fcf5017720fb65b79c9491f616a5182188e84d2f381e"), // 25
    hex("360f75419e04e42a58a0d78c22604384252a22ee3ed916f27bb58213a6c7a2f6"), // 26
    hex("918b73ef50bb76eae29d13e48424414ec19a22d28e6320dc29b9dfdb46bb64f5"), // 27
    hex("351c2f77aa610aca9bc7a254012bb4f28306629fd325eac28ce4d86a8134cc87"), // 28
    hex("29ce9c0920c4c950b22ba8230a38b17244d8896e4e7085a0009979d1907145eb"), // 29
    hex("89fce189eccf9b654a36b043f69ab0c61206f043a7588e126ad39152821e9f19"), // 30
    hex("0272cb60254c39c59f0e5825a22759c1d27a74dd89d764073a630d9c8b2dc224"), // 31
    hex("554b9ba5ba86ca380ac4bac11f2653cb6b2aa02bedcc46d41b640a9663791ba5"), // 32
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{Digest, Sha256};
    use alloc::{vec, vec::Vec};

    /// postcard writes a [`NodeHash`] exactly like the `Vec<u8>` it replaced
    /// (length, then raw bytes), so every commitment computed over a postcard
    /// encoding (`TraceCommitmentHeader::digest`, `StorageEntry::to_bytes`,
    /// `revealed_tail_roots_commitment`) is unchanged by the switch.
    #[test]
    fn hash32_postcard_matches_vec_u8() {
        let hashes: Vec<[u8; HASH_SIZE]> = (0..3u8).map(|n| [n; HASH_SIZE]).collect();
        let as_vecs: Vec<Vec<u8>> = hashes.iter().map(|h| h.to_vec()).collect();
        let as_node_hashes: Vec<NodeHash> = hashes.iter().map(|h| (*h).into()).collect();
        assert_eq!(
            postcard::to_allocvec(&as_node_hashes).unwrap(),
            postcard::to_allocvec(&as_vecs).unwrap()
        );
    }

    /// Decoding enforces the length: a 31-byte "hash" is rejected, not padded.
    #[test]
    fn hash32_rejects_wrong_length() {
        let short = postcard::to_allocvec(&vec![7u8; HASH_SIZE - 1]).unwrap();
        assert!(postcard::from_bytes::<NodeHash>(&short).is_err());
    }

    /// Recompute the recurrence from scratch. The host and guest tests pin
    /// the table to their own `combine`; this one pins it to the definition.
    #[test]
    fn empty_roots_follow_the_recurrence() {
        let mut root = EMPTY_LEAF;
        for (level, expected) in EMPTY_ROOTS.iter().enumerate() {
            assert_eq!(&root, expected, "EMPTY_ROOTS[{level}]");
            root = Sha256::new()
                .chain_update([level as u8])
                .chain_update(root)
                .chain_update(root)
                .finalize()
                .into();
        }
    }
}
