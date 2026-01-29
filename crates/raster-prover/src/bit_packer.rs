//! Bit packing utilities for compact fingerprints.
//!
//! This module provides the `BitPacker` type for packing hash bits into
//! compact fingerprints that can be efficiently compared.

use crate::error::{BitPackerError, Result};

/// Trait for cropping byte vectors to a specific bit length.
pub trait Crop {
    /// Crop the bytes to contain only the specified number of bits.
    fn crop(&self, size: usize) -> Vec<u8>;
}

impl Crop for Vec<u8> {
    fn crop(&self, size: usize) -> Vec<u8> {
        let rem_bits = size % 8;
        let bytes_size = (size / 8) + !size.is_multiple_of(8) as usize;
        let rem_mask: u8 = (1u8 << rem_bits) - 1;

        let mut cropped: Vec<u8> = Vec::with_capacity(bytes_size);
        cropped.extend_from_slice(&self[..bytes_size]);

        if rem_bits != 0 {
            if let Some(first) = cropped.iter_mut().last() {
                *first &= rem_mask;
            }
        } 

        cropped
    }
}

/// Packs hash bits into compact fingerprints.
///
/// The BitPacker extracts a fixed number of bits from each hash and packs
/// them into u64 blocks for efficient comparison.
///
/// # Example
///
/// ```
/// use bphc::bit_packer::BitPacker;
///
/// let bp = BitPacker::new(8); // 8 bits per item
/// let hashes = vec![vec![0u8; 32], vec![1u8; 32]];
/// let packed = bp.pack(&hashes);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct BitPacker(pub usize);

impl BitPacker {
    /// Create a new BitPacker with the specified bits per item.
    pub fn new(bits_per_item: usize) -> Self {
        Self(bits_per_item)
    }

    /// Get the number of bits per item.
    pub fn bits_per_item(&self) -> usize {
        self.0
    }

    /// Pack a list of hash bytes into compact u64 blocks.
    ///
    /// Each hash is cropped to `bits_per_item` bits and packed into
    /// consecutive positions in the resulting u64 vector.
    pub fn pack(&self, items: &[Vec<u8>]) -> Vec<u64> {
        let bits: Vec<Vec<u8>> = items.iter().map(|item| item.crop(self.0)).collect();

        let num_bits = items.len() * self.0;
        let num_blocks = (num_bits / 64) + !num_bits.is_multiple_of(64) as usize;

        let mut blocks = vec![0u64; num_blocks];
        if num_blocks != 0 {
            for (i, item_bits) in bits.iter().enumerate() {
                let global_offset = i * self.0;

                let block_idx = global_offset / 64;
                let block_offset = global_offset % 64;

                let mut item_bytes = [0u8; 8];
                let bytes_len = item_bits.len().min(8);
                item_bytes[..bytes_len].copy_from_slice(&item_bits[..bytes_len]);
                let item_u64 = u64::from_le_bytes(item_bytes);

                let overflow = (block_offset + self.0).saturating_sub(64);

                blocks[block_idx] |= item_u64 << block_offset;

                if overflow != 0 {
                    blocks[block_idx + 1] |= item_u64 >> (self.0 - overflow);
                }
            }
        }

        blocks
    }

    /// Get a value at the specified index.
    ///
    /// Returns None if the index would be out of bounds.
    pub fn get(&self, index: usize, packed: &[u64]) -> Option<u64> {
        self.try_get(index, packed).ok()
    }

    /// Try to get a value at the specified index.
    ///
    /// Returns an error if the index is out of bounds.
    pub fn try_get(&self, index: usize, packed: &[u64]) -> Result<u64> {
        let mut value = 0u64;

        let value_start_offset = index * self.0;
        let block_index = value_start_offset / 64;
        let value_end_offset = value_start_offset + self.0 - 1;
        let max_bits = packed.len() * 64;

        if value_end_offset >= max_bits {
            return Err(BitPackerError::IndexOutOfBounds {
                index,
                max: max_bits / self.0,
            });
        }

        let intra_block_start_index = value_start_offset % 64;
        let block = packed.get(block_index).ok_or(BitPackerError::IndexOutOfBounds {
            index: block_index,
            max: packed.len(),
        })?;

        let block_end_offset = ((block_index + 1) * 64) - 1;
        let block_value_bit_len = self.0 - value_end_offset.saturating_sub(block_end_offset);
        let mask = ((1u64 << block_value_bit_len) - 1u64) << intra_block_start_index;

        value |= (mask & *block) >> intra_block_start_index;

        if value_end_offset.saturating_sub(block_end_offset) > 0 {
            let next_block_index = block_index + 1;
            let next_block = packed.get(next_block_index).ok_or(BitPackerError::IndexOutOfBounds {
                index: next_block_index,
                max: packed.len(),
            })?;

            let next_block_value_bit_len = self.0 - block_value_bit_len;
            let mask = (1u64 << next_block_value_bit_len) - 1u64;

            value |= (mask & *next_block) << block_value_bit_len;
        }

        Ok(value)
    }

    /// Get a range of packed values.
    ///
    /// Returns None if the range is invalid.
    pub fn get_range(&self, start: usize, end: usize, packed: &[u64]) -> Option<Vec<u64>> {
        self.try_get_range(start, end, packed).ok()
    }

    /// Try to get a range of packed values.
    ///
    /// Returns an error if the range is invalid.
    pub fn try_get_range(&self, start: usize, end: usize, packed: &[u64]) -> Result<Vec<u64>> {
        if start >= end {
            return Err(BitPackerError::InvalidRange {
                start,
                end,
                max: packed.len() * 64 / self.0,
            });
        }

        let num_bits = (end - start) * self.0;
        let num_blocks = (num_bits / 64) + (!num_bits.is_multiple_of(64)) as usize;

        let start_offset = start * self.0;
        let end_offset = (end * self.0) + self.0 - 1;
        let max_bits = packed.len() * 64;

        if start_offset > max_bits || end_offset > max_bits {
            return Err(BitPackerError::InvalidRange {
                start,
                end,
                max: max_bits / self.0,
            });
        }

        let mut range: Vec<u64> = vec![0u64; num_blocks];

        for (i, index) in (start..end).enumerate() {
            let bit_start = i * self.0;
            let bit_end = bit_start + self.0 - 1;

            let block_idx = bit_start / 64;
            let block_offset = bit_start % 64;

            if let Some(value) = self.get(index, packed) {
                if bit_end < (block_idx + 1) * 64 {
                    range[block_idx] |= value << block_offset;
                } else {
                    let next_block_value_bit_len = bit_end - ((block_idx + 1) * 64);
                    let current_block_value_bit_len = self.0 - next_block_value_bit_len;
                    range[block_idx] |=
                        (value & ((1u64 << current_block_value_bit_len) - 1u64)) << block_offset;

                    let overflow_bits = value >> current_block_value_bit_len;
                    range[block_idx + 1] =
                        range[block_idx + 1] << next_block_value_bit_len | overflow_bits;
                }
            }
        }
        Ok(range)
    }

    /// Find the first difference between two packed arrays.
    ///
    /// Returns the position and values at that position, or None if equal.
    pub fn diff(&self, l_bits: &[u64], r_bits: &[u64]) -> Option<(usize, u64, u64)> {
        self.try_diff(l_bits, r_bits).ok().flatten()
    }

    /// Try to find the first difference between two packed arrays.
    ///
    /// Returns an error if the arrays have different lengths.
    pub fn try_diff(&self, l_bits: &[u64], r_bits: &[u64]) -> Result<Option<(usize, u64, u64)>> {
        if l_bits.len() != r_bits.len() {
            return Err(BitPackerError::LengthMismatch {
                expected: l_bits.len(),
                actual: r_bits.len(),
            });
        }

        for (i, (block, other_block)) in l_bits.iter().zip(r_bits.iter()).enumerate() {
            let block_bytes = block.to_le_bytes();
            let other_block_bytes = other_block.to_le_bytes();

            for (byte_num, (self_byte, other_byte)) in
                block_bytes.iter().zip(other_block_bytes.iter()).enumerate()
            {
                if self_byte != other_byte {
                    for bit in 0..8 {
                        if (self_byte >> bit & 1u8) != (other_byte >> bit & 1u8) {
                            let diff_bit = bit + (byte_num * 8) + (i * 64);
                            let diff_pos = diff_bit / self.0;

                            let l_value = self.try_get(diff_pos, l_bits)?;
                            let r_value = self.try_get(diff_pos, r_bits)?;

                            return Ok(Some((diff_pos, l_value, r_value)));
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// Create an iterator over the packed values.
    pub fn iter<'a, 'b>(&'a self, packed: &'b [u64]) -> Iter<'a, 'b> {
        Iter {
            bp: self,
            packed,
            index: 0,
        }
    }
}

/// Iterator over packed values.
pub struct Iter<'a, 'b> {
    bp: &'a BitPacker,
    packed: &'b [u64],
    index: usize,
}

impl<'a, 'b> Iterator for Iter<'a, 'b> {
    type Item = u64;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.packed.len() {
            return None;
        }

        let value = self.bp.get(self.index, self.packed);
        self.index += 1;

        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_crop_lt_8() {
        let mut bytes: [u8; 32] = [0u8; 32];
        bytes[0] = 0b10010101u8;

        let random_hash = bytes.to_vec();

        let bits_n = 5;
        let result_5 = random_hash.crop(bits_n);
        assert_eq!(result_5, vec![0b00010101]);

        let bits_n = 4;
        let result_4 = random_hash.crop(bits_n);
        assert_eq!(result_4, vec![0b00000101]);

        let bits_n = 3;
        let result_3 = random_hash.crop(bits_n);
        assert_eq!(result_3, vec![0b00000101]);

        let bits_n = 2;
        let result_2 = random_hash.crop(bits_n);
        assert_eq!(result_2, vec![0b00000001]);
    }

    #[test]
    fn should_crop_gt_8() {
        let mut bytes: [u8; 32] = [0u8; 32];
        bytes[0] = 0b10000001u8;
        bytes[1] = 0b10000001u8;
        bytes[2] = 0b10000001u8;

        let random_hash = bytes.to_vec();

        let bits_n = 9;
        let result_9 = random_hash.crop(bits_n);
        assert_eq!(result_9, vec![0b10000001, 0b00000001]);

        let bits_n = 10;
        let result_10 = random_hash.crop(bits_n);
        assert_eq!(result_10, vec![0b10000001, 0b00000001]);

        let bits_n = 16;
        let result_16 = random_hash.crop(bits_n);
        assert_eq!(result_16, vec![0b10000001, 0b10000001]);

        let bits_n = 17;
        let result_17 = random_hash.crop(bits_n);
        assert_eq!(result_17, vec![0b10000001, 0b10000001, 0b00000001],);

        let bits_n = 24;
        let result_24 = random_hash.crop(bits_n);
        assert_eq!(result_24, vec![0b10000001, 0b10000001, 0b10000001],);
    }

    #[test]
    fn should_pack_1_bit() {
        let fingerprints: Vec<Vec<u8>> = vec![[0u8; 32].to_vec(), [1u8; 32].to_vec()];

        let expected_packed: Vec<u64> = vec![0b10u64];

        let bp = BitPacker(1);

        let packed = bp.pack(&fingerprints);

        assert_eq!(expected_packed, packed);
    }

    #[test]
    fn should_pack_32_bit() {
        let fingerprints: Vec<Vec<u8>> = vec![[0u8; 32].to_vec(), [0b11111111u8; 32].to_vec()];

        let expected_packed: Vec<u64> =
            vec![0b1111111111111111111111111111111100000000000000000000000000000000u64];

        let bp = BitPacker(32);

        let packed = bp.pack(&fingerprints);

        assert_eq!(expected_packed, packed);
    }

    #[test]
    fn should_pack_2_bit() {
        let fingerprints: Vec<Vec<u8>> = vec![
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
        ];

        let expected_packed: Vec<u64> = vec![0b11001100u64];

        let bp = BitPacker(2);

        let packed = bp.pack(&fingerprints);

        assert_eq!(expected_packed, packed);
    }

    #[test]
    fn should_pack_le() {
        let fingerprints: Vec<Vec<u8>> = vec![
            [1u8; 32].to_vec(),
            [2u8; 32].to_vec(),
            [3u8; 32].to_vec(),
            [4u8; 32].to_vec(),
            [5u8; 32].to_vec(),
            [6u8; 32].to_vec(),
            [7u8; 32].to_vec(),
            [8u8; 32].to_vec(),
            [9u8; 32].to_vec(),
            [10u8; 32].to_vec(),
            [11u8; 32].to_vec(),
            [12u8; 32].to_vec(),
        ];

        let expected_packed: Vec<u64> = vec![
            u64::from_le_bytes(
                [1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8]
                    .try_into()
                    .expect("slice with incorrect length"),
            ),
            u64::from_le_bytes(
                [9u8, 10u8, 11u8, 12u8, 0u8, 0u8, 0u8, 0u8]
                    .try_into()
                    .expect("slice with incorrect length"),
            ),
        ];

        let bp = BitPacker(8);

        let packed = bp.pack(&fingerprints);

        assert_eq!(expected_packed, packed);
    }
    #[test]
    fn should_pack_9_bit() {
        let fingerprints: Vec<Vec<u8>> = vec![
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b11111111u8; 32].to_vec(),
        ];

        let expected_packed: Vec<u64> = vec![
            0b1000000000111111111000000000111111111000000000111111111000000000u64,
            0b0000000000000000000011111111100000000011111111100000000011111111u64,
        ];

        let bp = BitPacker(9);

        let packed = bp.pack(&fingerprints);

        assert_eq!(expected_packed, packed);
    }

    #[test]
    fn should_get_value() {
        let values_range = 0..10;
        let fingerprints: Vec<Vec<u8>> = values_range
            .clone()
            .map(|n| {
                [
                    n as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                ]
                .to_vec()
            })
            .collect();

        for pack_size in 1..10 {
            let bp = BitPacker(pack_size);

            let packed = bp.pack(&fingerprints);

            let expected_value_mask = (1u64 << pack_size) - 1;
            for (j, expected_value) in values_range.clone().enumerate() {
                let value = bp.get(j, &packed).expect("expected value {i}");
                assert_eq!(
                    expected_value as u64 & expected_value_mask,
                    value,
                    "not valid value"
                );
            }
        }
    }

    #[test]
    fn should_get_range() {
        let values_range = 0..10;
        let fingerprints: Vec<Vec<u8>> = values_range.clone().map(|n| [n; 32].to_vec()).collect();

        let pack_size = 1;
        let bp = BitPacker(pack_size);

        let packed_fingerprints = bp.pack(&fingerprints);

        let range = bp
            .get_range(0, 8, &packed_fingerprints)
            .expect("invalid slice");

        for (i, value) in bp.iter(&range).enumerate() {
            assert_eq!(i as u64 & ((1u64 << pack_size) - 1), value);
        }
    }

    #[test]
    fn should_get_range_fit_single_block() {
        let values_range = 0..100;
        let fingerprints: Vec<Vec<u8>> = values_range.clone().map(|n| [n; 32].to_vec()).collect();

        let pack_size = 8;
        let bp = BitPacker(pack_size);

        let packed_fingerprints = bp.pack(&fingerprints);

        let range = bp
            .get_range(0, 8, &packed_fingerprints)
            .expect("invalid slice");

        for (i, value) in bp.iter(&range).enumerate() {
            assert_eq!(i as u64 & ((1u64 << pack_size) - 1), value);
        }
    }
    #[test]
    fn should_get_range_pack_power_of_2() {
        let values_range = 0..100;
        let fingerprints: Vec<Vec<u8>> = values_range.clone().map(|n| [n; 32].to_vec()).collect();

        let pack_size = 16;
        let bp = BitPacker(pack_size);

        let packed_fingerprints = bp.pack(&fingerprints);

        let range = bp
            .get_range(0, 8, &packed_fingerprints)
            .expect("invalid slice");

        let u64_values: Vec<u64> = fingerprints
            .iter()
            .map(|bytes| {
                u64::from_le_bytes(bytes[0..8].try_into().expect("slice with incorrect length"))
            })
            .collect();

        for (i, value) in bp.iter(&range).enumerate() {
            assert_eq!(u64_values[i] & ((1u64 << pack_size) - 1), value);
        }
    }

    #[test]
    fn should_get_range_pack_not_power_of_2() {
        let values_range = 0..100;
        let fingerprints: Vec<Vec<u8>> = values_range.clone().map(|n| [n; 32].to_vec()).collect();

        let pack_size = 9;
        let bp = BitPacker(pack_size);

        let packed_fingerprints = bp.pack(&fingerprints);

        let range_size = 8;
        let range = bp
            .get_range(0, range_size, &packed_fingerprints)
            .expect("invalid slice");

        let u64_values: Vec<u64> = fingerprints
            .iter()
            .map(|bytes| {
                u64::from_le_bytes(bytes[0..8].try_into().expect("slice with incorrect length"))
            })
            .collect();

        assert_eq!(
            range.len(),
            ((pack_size * range_size) / 64) + ((pack_size * range_size) % 64 != 0) as usize
        );
        for (i, value) in bp.iter(&range).enumerate() {
            assert_eq!(u64_values[i] & ((1u64 << pack_size) - 1), value);
        }
    }

    #[test]
    fn should_iter_through_packed() {
        let values_range = 0..10;
        let fingerprints: Vec<Vec<u8>> = values_range.clone().map(|n| [n; 32].to_vec()).collect();

        let bp = BitPacker(4);

        let packed_fingerprints = bp.pack(&fingerprints);

        for (i, value) in bp.iter(&packed_fingerprints).enumerate() {
            assert_eq!(i as u64, value);
        }
    }
    #[test]
    fn should_get_diff_le() {
        let fingerprints: Vec<Vec<u8>> = vec![
            [1u8; 32].to_vec(),
            [2u8; 32].to_vec(),
            [3u8; 32].to_vec(),
            [4u8; 32].to_vec(),
            [5u8; 32].to_vec(),
            [6u8; 32].to_vec(),
            [7u8; 32].to_vec(),
            [8u8; 32].to_vec(),
            [9u8; 32].to_vec(),
            [10u8; 32].to_vec(),
            [11u8; 32].to_vec(),
            [12u8; 32].to_vec(),
        ];

        let other_fingerprints: Vec<Vec<u8>> = vec![
            [1u8; 32].to_vec(),
            [0u8; 32].to_vec(),
            [3u8; 32].to_vec(),
            [4u8; 32].to_vec(),
            [5u8; 32].to_vec(),
            [6u8; 32].to_vec(),
            [7u8; 32].to_vec(),
            [8u8; 32].to_vec(),
            [9u8; 32].to_vec(),
            [10u8; 32].to_vec(),
            [11u8; 32].to_vec(),
            [12u8; 32].to_vec(),
        ];
        let bp = BitPacker(8);

        let packed = bp.pack(&fingerprints);
        let other_packed = bp.pack(&other_fingerprints);

        if let Some((diff_pos, l_value, r_value)) = bp.diff(&packed, &other_packed) {
            assert_eq!(diff_pos, 1, "wrong diff position");
            assert_eq!(l_value, 2, "wrong left value");
            assert_eq!(r_value, 0, "wrong right value");
        }
    }

    #[test]
    fn should_get_diff() {
        let fingerprints: Vec<Vec<u8>> = vec![
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
            [0b00000000u8; 32].to_vec(),
        ];

        for fingerprint_bits in 1..9 {
            let bp = BitPacker(fingerprint_bits);
            let packed_fingerprints = bp.pack(&fingerprints);

            for i in 0..fingerprints.len() {
                let ref_fingerprints: Vec<Vec<u8>> = fingerprints
                    .iter()
                    .enumerate()
                    .map(|(j, _fingerprint)| {
                        if i == j {
                            [0b11111111u8; 32].to_vec()
                        } else {
                            [0b00000000u8; 32].to_vec()
                        }
                    })
                    .collect();

                let packed_ref_fingerprints = bp.pack(&ref_fingerprints);

                let (diff_index, value, ref_value) = bp
                    .diff(&packed_fingerprints, &packed_ref_fingerprints)
                    .expect("no diff");
                assert_eq!(i, diff_index, "wrong diff index: {i}");

                assert_eq!(0b0u64, value, "wrong diff at index {i}");
                assert_eq!(
                    (1u64 << fingerprint_bits) - 1,
                    ref_value,
                    "wrong diff at index {i}"
                );
            }
        }
    }
}
