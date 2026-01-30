//! Precomputed constants for the Merkle tree.
//!
//! This module contains precomputed empty node hashes for the incremental
//! Merkle tree, allowing efficient initialization without computing the
//! full tree of empty values.

use hex_literal::hex;

/// 256-bit hash type.
pub type H256 = [u8; 32];

/// Size of a hash in bytes (SHA256).
pub const HASH_SIZE: usize = 32;

/// Precomputed empty trie nodes at each level.
///
/// These are the hashes of empty subtrees at each level of the Merkle tree.
/// Used for efficient tree initialization and sparse tree representation.
///
/// - Level 0: Hash of empty leaf
/// - Level N: Hash of (level, empty_node[N-1], empty_node[N-1])
pub const EMPTY_TRIE_NODES: [H256; 32] = [
    hex!("6d97a6c02676a41a9636c6cd4e5d2d47d14d27a35d18e608115fd93cd42e6b3a"),
    hex!("aee2052a16c7b279ee4a2faab612c49536cb1871c08979ba0a05a2d17136a408"),
    hex!("6381c52cb389072848c667a0a0527dc6f64d35566e64455f9447592cfd756158"),
    hex!("dbabc0ff00a916ade80c3e84d768b6f14002321ed1511ac766d33c824ceee398"),
    hex!("62e86b1c49154c89ce1866882f4b8b1c41d7bc9c60999fec7d45e3b77f4aa445"),
    hex!("ab74d3c639033350773d52779d6f75ab56e100c6f3e967efadc7960a00692076"),
    hex!("30aab9e6966c87ed793e2155575a49cd6207d324e684668d0d15657c6fb3c8b9"),
    hex!("396e8cadd9b0c02593d43ad25c9492f4aef3c90306b86872377ea441a0b762c8"),
    hex!("4c101452ed6a644d27a500fcad8c8ab3c2c229b70e438fd02fda777f718ce203"),
    hex!("2ce13b1149561b9eec43b9b5fa8aaa8df9500f5fc3952e8ab957d2f94256969c"),
    hex!("5c92ba300a8123b9c93b8dcd2ad2b88d84a583c6a3723cced7f14c3ce2c87d51"),
    hex!("fb40b43e8f6bce9cf95a44b7e53b95d7e628519afe93bde07e156f31529ecf32"),
    hex!("fc45d0b150d8d1ef5f6fd35f686d3acb32a74b59f6596a561405bfaa6820fefa"),
    hex!("ddbe5d4583fb79fa95be3c41796651a544866600c60d80d44a4d8daf736ebb77"),
    hex!("19c28ed710fcdf003af6a16cd7e6ecdf7725dd82b9831d98d482574b373f7555"),
    hex!("6cf2d44e7fc7dba947fbf83cf8590bd9ad4eec9cc9a2c39a6c4ff00e86bf18b4"),
    hex!("d30da6b272332b244619a6764d9fc8810183d74297a899bb9f0759a432781ab7"),
    hex!("772413ef67adfb21af279b7ca35179e0566a81282a55a529754c4367f6387ab0"),
    hex!("41dd7b7946a5fef21628b928b4f033339ad85ef4fcc10dd593d2462204b9a568"),
    hex!("2cb3baca1ef218ec96bc89fefa597d7a3ad9ad3b7bf0d0b53047c5243fdd3277"),
    hex!("cfeee7497f0f7ca1082f684f395b506612f16a8746def22953f1c8062ff43be7"),
    hex!("ca70304bb20c920d0763a68db49be46cc76b4ccaa49d1bd0bbd57db0acec87bc"),
    hex!("119a2d6681d794d7508c5830b430548aa188f0040097127df118616e5d2f1574"),
    hex!("6db969708f15c30e218fb04078b0d0d4771e99e68148b7d044a8ad9b98e46c95"),
    hex!("2c20dc7bf0d0e351412be6b506300bef26fef1fd5f2cf2bcd50da2b40eaaeaf5"),
    hex!("12f8835c2ccc2b7c6325e58b4b6de5c071bc28f55ee2444490f5fe34a4c85f06"),
    hex!("9de929d5a2e0e97ff05b8ad7842af4aaacc143cae6ae543a4cd9ae647b001efa"),
    hex!("7de518303b3c72cf44eba70a56905171812340365ffeab6b91ef4a824f0b917b"),
    hex!("5912d6d1a721faece6df02e2cf8a93191496f91b39cdd35f74f46f8d26a7b91a"),
    hex!("cc6d01b4829c40e09eeb0c1d6510d793b10fa722bad154b9ac60b4e789ea84a4"),
    hex!("26a0de765fff1b499936fea70c937718e62a2649e1c156c2d5c4e43c87a14621"),
    hex!("6ece6d2a701e0b70fb9484e77df836510a6a7c0b4b7aef8c8e7a2a9414e54aba"),
];
