//! Bit packing utilities for compact fingerprints.
//!
//! This module provides the `BitPacker` type for packing hash bits into
//! compact fingerprints that can be efficiently compared.

use std::fmt;
use std::result::Result;
use std::string::String;
use std::vec;
use std::vec::Vec;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub enum BitPackerError {
    /// BitPacker index is out of bounds.
    IndexOutOfBounds { index: usize, max: usize },
    /// Invalid range for BitPacker operations.
    InvalidRange {
        start: usize,
        end: usize,
        max: usize,
    },
    /// Arrays have different lengths in comparison.
    LengthMismatch { expected: usize, actual: usize },
    /// Failed to serialize data.
    SerializationError(String),
}

impl fmt::Display for BitPackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitPackerError::IndexOutOfBounds { index, max } => {
                write!(f, "Index {} out of bounds (max: {})", index, max)
            }
            BitPackerError::InvalidRange { start, end, max } => {
                write!(f, "Invalid range [{}, {}) for max {}", start, end, max)
            }
            BitPackerError::LengthMismatch { expected, actual } => {
                write!(f, "Length mismatch: expected {}, got {}", expected, actual)
            }
            BitPackerError::SerializationError(msg) => {
                write!(f, "Serialization error: {}", msg)
            }
        }
    }
}

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
/// use raster_core::fingerprint::BitPacker;
///
/// let bp = BitPacker::new(8); // 8 bits per item
/// let hashes = vec![vec![0u8; 32], vec![1u8; 32]];
/// let packed = bp.pack(&hashes);
/// ```
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
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
    pub fn try_get(&self, index: usize, packed: &[u64]) -> Result<u64, BitPackerError> {
        let bit_width = self.0;

        let value_start_offset = index * bit_width;
        let value_end_offset = value_start_offset + bit_width; // Exclusive end
        let max_bits = packed.len() * 64;

        if value_end_offset > max_bits {
            return Err(BitPackerError::IndexOutOfBounds {
                index,
                max: max_bits / bit_width,
            });
        }

        let block_index = value_start_offset / 64;
        let intra_block_offset = value_start_offset % 64;

        let mask = if bit_width == 64 {
            !0u64
        } else {
            (1u64 << bit_width) - 1
        };

        let mut value = packed[block_index] >> intra_block_offset;

        let bits_in_first_block = 64 - intra_block_offset;
        if bits_in_first_block < bit_width {
            let next_block = packed[block_index + 1];
            value |= next_block << bits_in_first_block;
        }

        Ok(value & mask)
    }

    /// Get a range of packed values.
    ///
    /// Returns None if the range is invalid.
    pub fn get_range(&self, start: usize, end: usize, packed: &[u64]) -> Option<Vec<u64>> {
        self.try_get_range(start, end, packed).ok()
    }

    pub fn try_get_range(
        &self,
        start: usize,
        end: usize,
        packed: &[u64],
    ) -> Result<Vec<u64>, BitPackerError> {
        let bit_width = self.0;
        if start >= end {
            return Ok(Vec::new());
        }

        let total_bits_needed = end * bit_width;
        let max_bits_available = packed.len() * 64;

        if total_bits_needed > max_bits_available {
            return Err(BitPackerError::InvalidRange {
                start,
                end,
                max: max_bits_available / bit_width,
            });
        }

        let num_elements = end - start;
        let total_output_bits = num_elements * bit_width;
        let num_blocks = total_output_bits.div_ceil(64);
        let mut range = vec![0u64; num_blocks];

        for (i, index) in (start..end).enumerate() {
            let value = self.try_get(index, packed)?;

            let bit_start = i * bit_width;
            let block_idx = bit_start / 64;
            let block_offset = bit_start % 64;

            range[block_idx] |= value << block_offset;

            let bits_written = 64 - block_offset;
            if bits_written < bit_width {
                range[block_idx + 1] |= value >> bits_written;
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

    pub fn diff_at_index(&self, index: usize, l_bits: &[u64], r_bits: &[u64]) -> bool {
        self.try_diff_at_index(index, l_bits, r_bits).unwrap()
    }

    pub fn try_diff_at_index(
        &self,
        index: usize,
        l_bits: &[u64],
        r_bits: &[u64],
    ) -> Result<bool, BitPackerError> {
        let l_value = self.try_get(index, l_bits)?;
        let r_value = self.try_get(index, r_bits)?;

        if l_value != r_value {
            return Ok(true);
        }

        Ok(false)
    }

    /// Try to find the first difference between two packed arrays.
    ///
    /// Returns an error if the arrays have different lengths.
    pub fn try_diff(
        &self,
        l_bits: &[u64],
        r_bits: &[u64],
    ) -> Result<Option<(usize, u64, u64)>, BitPackerError> {
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
                            let diff_bit_index = bit + (byte_num * 8) + (i * 64);
                            let diff_pos = diff_bit_index / self.0;

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

/// Streaming bit packer with a 2-block sliding window and callback-based block emission.
///
/// Unlike `BitPacker` which requires all items upfront, `StreamingBitPacker` allows
/// iterative item addition. When the first block (64 bits) is filled, it is emitted
/// via the callback and the window shifts.
///
/// # Example
///
/// ```
/// use raster_core::fingerprint::StreamingBitPacker;
///
/// let mut emitted_blocks = Vec::new();
/// {
///     let mut packer = StreamingBitPacker::new(8, |block| {
///         emitted_blocks.push(block);
///     });
///
///     // Push 8 items of 8 bits each = 64 bits = 1 full block
///     for i in 0..8u8 {
///         packer.push(&[i]);
///     }
///
///     // Get remaining blocks
///     let (remaining0, remaining1) = packer.finish();
/// }
/// ```
pub struct StreamingBitPacker<F>
where
    F: FnMut(u64),
{
    bits_per_item: usize,
    blocks: [u64; 2],     // Sliding window of 2 blocks
    bit_offset: usize,    // Current bit position (0-127)
    on_block_complete: F, // Callback when block 0 is filled
}

impl<F> StreamingBitPacker<F>
where
    F: FnMut(u64),
{
    /// Create a new StreamingBitPacker with the specified bits per item.
    ///
    /// # Arguments
    ///
    /// * `bits_per_item` - Number of bits to use per item (must be <= 64)
    /// * `on_block_complete` - Callback invoked when a 64-bit block is complete
    ///
    /// # Panics
    ///
    /// Panics if `bits_per_item` is 0 or greater than 64.
    pub fn new(bits_per_item: usize, on_block_complete: F) -> Self {
        assert!(
            bits_per_item > 0 && bits_per_item <= 64,
            "bits_per_item must be between 1 and 64"
        );
        Self {
            bits_per_item,
            blocks: [0u64; 2],
            bit_offset: 0,
            on_block_complete,
        }
    }

    /// Push an item into the packer.
    ///
    /// The item is cropped to `bits_per_item` bits and packed into the current
    /// window position. When the first block (64 bits) is filled, it is emitted
    /// via the callback and the window shifts.
    ///
    /// # Arguments
    ///
    /// * `item` - The bytes to pack (will be cropped to `bits_per_item` bits)
    pub fn push(&mut self, item: &[u8]) {
        // Crop the item to the specified number of bits
        let cropped = item.to_vec().crop(self.bits_per_item);

        // Convert cropped bytes to u64 (little-endian)
        let mut item_bytes = [0u8; 8];
        let bytes_len = cropped.len().min(8);
        item_bytes[..bytes_len].copy_from_slice(&cropped[..bytes_len]);
        let item_u64 = u64::from_le_bytes(item_bytes);

        // Calculate position within the 2-block window
        let block_offset = self.bit_offset % 64;

        // Pack into the current position
        // If bit_offset < 64, we're in block 0; otherwise we're in block 1
        if self.bit_offset < 64 {
            // Writing to block 0
            self.blocks[0] |= item_u64 << block_offset;

            // Check for overflow into block 1
            let overflow = (block_offset + self.bits_per_item).saturating_sub(64);
            if overflow != 0 {
                self.blocks[1] |= item_u64 >> (self.bits_per_item - overflow);
            }
        } else {
            // Writing to block 1
            self.blocks[1] |= item_u64 << block_offset;
        }

        // Advance bit offset
        self.bit_offset += self.bits_per_item;

        // Check if block 0 is complete (we've written past 64 bits)
        if self.bit_offset >= 64 {
            // Only emit and shift if we just crossed the 64-bit boundary
            // or if we're now at 128 bits (block 1 is full)
            if self.bit_offset >= 64 && self.bit_offset - self.bits_per_item < 64 {
                // We just completed block 0, emit it
                (self.on_block_complete)(self.blocks[0]);

                // Shift: block 1 becomes block 0
                self.blocks[0] = self.blocks[1];
                self.blocks[1] = 0;
                self.bit_offset -= 64;
            } else if self.bit_offset >= 128 {
                // Block 1 is also full, emit block 0 (which was block 1)
                (self.on_block_complete)(self.blocks[0]);

                // Shift again
                self.blocks[0] = self.blocks[1];
                self.blocks[1] = 0;
                self.bit_offset -= 64;
            }
        }
    }

    /// Finish packing and return any remaining data in the blocks.
    ///
    /// # Returns
    ///
    /// A tuple of two optional u64 values:
    /// - First: The remaining block 0 if `bit_offset > 0`
    /// - Second: The remaining block 1 if `bit_offset > 64`
    pub fn finish(self) -> (Option<u64>, Option<u64>) {
        if self.bit_offset == 0 {
            (None, None)
        } else if self.bit_offset <= 64 {
            (Some(self.blocks[0]), None)
        } else {
            (Some(self.blocks[0]), Some(self.blocks[1]))
        }
    }
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

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub bits_packer: BitPacker,
    pub bits: Vec<u64>,
    pub len: usize,
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for bit in self.bits.iter() {
            write!(f, "{:064b}", bit)?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for bit in self.bits.iter() {
            write!(f, "{:064b}", bit)?;
        }
        Ok(())
    }
}

impl Fingerprint {
    pub fn from(bits: Vec<u64>, bits_packer: BitPacker, len: usize) -> Self {
        Self {
            bits_packer,
            bits,
            len,
        }
    }

    pub fn diff_at_index(&self, index: usize, other: &Self) -> bool {
        assert!(
            self.len() > index && other.len() > index,
            "Index out of bounds"
        );

        self.bits_packer
            .diff_at_index(index, &self.bits, &other.bits)
    }

    pub fn bits_per_item(&self) -> usize {
        self.bits_packer.bits_per_item()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FingerprintAccumulator {
    fingerprint: Fingerprint,
}

impl From<Fingerprint> for FingerprintAccumulator {
    fn from(fingerprint: Fingerprint) -> Self {
        Self { fingerprint }
    }
}

impl FingerprintAccumulator {
    pub fn new(bits_packer: BitPacker) -> Self {
        Self {
            fingerprint: Fingerprint {
                bits_packer,
                bits: Vec::new(),
                len: 0,
            },
        }
    }

    pub fn append(&mut self, item: &[u8]) {
        // Crop the item to the specified number of bits
        let cropped = item.to_vec().crop(self.fingerprint.bits_per_item());

        // Convert cropped bytes to u64 (little-endian)
        let mut item_bytes = [0u8; 8];
        let bytes_len = cropped.len().min(8);
        item_bytes[..bytes_len].copy_from_slice(&cropped[..bytes_len]);
        let item_u64 = u64::from_le_bytes(item_bytes);

        // Calculate which block(s) the item spans
        let item_pos = self.fingerprint.len();
        let block_idx = (item_pos * self.fingerprint.bits_per_item()) / 64;
        let block_offset = (item_pos * self.fingerprint.bits_per_item()) % 64;

        // Ensure we have enough blocks (auto-grow)
        // We need at least block_idx + 1 blocks, and possibly block_idx + 2 if there's overflow
        let overflow =
            (block_offset + self.fingerprint.bits_packer.bits_per_item()).saturating_sub(64);
        let required_blocks = if overflow != 0 {
            block_idx + 2
        } else {
            block_idx + 1
        };

        if self.fingerprint.bits.len() < required_blocks {
            self.fingerprint.bits.resize(required_blocks, 0u64);
        }

        // Pack the bits (same logic as BitPacker::pack)
        self.fingerprint.bits[block_idx] |= item_u64 << block_offset;

        if overflow != 0 {
            self.fingerprint.bits[block_idx + 1] |=
                item_u64 >> (self.fingerprint.bits_packer.bits_per_item() - overflow);
        }

        // Advance bit offset
        self.fingerprint.len += 1;
    }

    pub fn len(&self) -> usize {
        self.fingerprint.len()
    }

    pub fn into_fingerprint(self) -> Fingerprint {
        self.fingerprint
    }

    pub fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    pub fn push(&mut self, item: &[u8]) {
        self.append(item);
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
    fn fingerprint_accumulator_push_matches_bit_packer_pack() {
        let items: Vec<Vec<u8>> = vec![
            vec![0b00000001u8; 32],
            vec![0b00000011u8; 32],
            vec![0b00000111u8; 32],
        ];
        let bp = BitPacker::new(8);
        let expected = bp.pack(&items);
        let fingerprint = FingerprintAccumulator::from(Fingerprint::from(expected.clone(), bp, items.len()));

        let mut acc = FingerprintAccumulator::new(bp);
        for item in &items {
            acc.push(item);
        }

        assert_eq!(acc.len(), 3);
        assert_eq!(acc, fingerprint);
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

    // StreamingBitPacker tests

    #[test]
    fn streaming_should_emit_block_when_full() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let emitted_blocks = Rc::new(RefCell::new(Vec::new()));
        let emitted_clone = emitted_blocks.clone();

        let mut packer = StreamingBitPacker::new(8, move |block| {
            emitted_clone.borrow_mut().push(block);
        });

        // Push 8 items of 8 bits each = 64 bits = 1 full block
        for i in 0..8u8 {
            packer.push(&[i]);
        }

        // Should have emitted 1 block
        assert_eq!(emitted_blocks.borrow().len(), 1);

        let (remaining0, remaining1) = packer.finish();
        assert!(remaining0.is_none());
        assert!(remaining1.is_none());

        // Verify the emitted block has the correct content
        let expected = u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(emitted_blocks.borrow()[0], expected);
    }

    #[test]
    fn streaming_should_return_remaining_in_finish() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let emitted_blocks = Rc::new(RefCell::new(Vec::new()));
        let emitted_clone = emitted_blocks.clone();

        let mut packer = StreamingBitPacker::new(8, move |block| {
            emitted_clone.borrow_mut().push(block);
        });

        // Push only 4 items = 32 bits, not enough for a full block
        for i in 0..4u8 {
            packer.push(&[i]);
        }

        // No blocks should have been emitted
        assert_eq!(emitted_blocks.borrow().len(), 0);

        let (remaining0, remaining1) = packer.finish();
        assert!(remaining0.is_some());
        assert!(remaining1.is_none());

        let expected = u64::from_le_bytes([0, 1, 2, 3, 0, 0, 0, 0]);
        assert_eq!(remaining0.unwrap(), expected);
    }

    #[test]
    fn streaming_matches_bitpacker_pack() {
        // Test that streaming produces the same result as batch packing
        let fingerprints: Vec<Vec<u8>> = (0..12u8).map(|n| [n; 32].to_vec()).collect();

        let bp = BitPacker(8);
        let expected_packed = bp.pack(&fingerprints);

        let mut emitted_blocks = Vec::new();
        {
            let mut packer = StreamingBitPacker::new(8, |block| {
                emitted_blocks.push(block);
            });

            for fp in &fingerprints {
                packer.push(fp);
            }

            let (remaining0, remaining1) = packer.finish();
            if let Some(block) = remaining0 {
                emitted_blocks.push(block);
            }
            if let Some(block) = remaining1 {
                emitted_blocks.push(block);
            }
        }

        assert_eq!(emitted_blocks, expected_packed);
    }

    #[test]
    fn streaming_9_bit_matches_bitpacker() {
        // Test with 9-bit items that span across block boundaries
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

        let bp = BitPacker(9);
        let expected_packed = bp.pack(&fingerprints);

        let mut emitted_blocks = Vec::new();
        {
            let mut packer = StreamingBitPacker::new(9, |block| {
                emitted_blocks.push(block);
            });

            for fp in &fingerprints {
                packer.push(fp);
            }

            let (remaining0, remaining1) = packer.finish();
            if let Some(block) = remaining0 {
                emitted_blocks.push(block);
            }
            if let Some(block) = remaining1 {
                emitted_blocks.push(block);
            }
        }

        assert_eq!(emitted_blocks, expected_packed);
    }

    #[test]
    fn streaming_empty_finish() {
        let packer = StreamingBitPacker::new(8, |_block| {
            panic!("should not emit any blocks");
        });

        let (remaining0, remaining1) = packer.finish();
        assert!(remaining0.is_none());
        assert!(remaining1.is_none());
    }

    #[test]
    fn streaming_multiple_block_emissions() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let emitted_blocks = Rc::new(RefCell::new(Vec::new()));
        let emitted_clone = emitted_blocks.clone();

        let mut packer = StreamingBitPacker::new(8, move |block| {
            emitted_clone.borrow_mut().push(block);
        });

        // Push 20 items = 160 bits = 2 full blocks + 32 bits remaining
        for i in 0..20u8 {
            packer.push(&[i]);
        }

        // Should have emitted 2 blocks
        assert_eq!(emitted_blocks.borrow().len(), 2);

        let (remaining0, remaining1) = packer.finish();
        assert!(remaining0.is_some());
        assert!(remaining1.is_none());

        // Add remaining to emitted for comparison
        if let Some(block) = remaining0 {
            emitted_blocks.borrow_mut().push(block);
        }

        // Verify against BitPacker
        let fingerprints: Vec<Vec<u8>> = (0..20u8).map(|n| [n; 32].to_vec()).collect();
        let bp = BitPacker(8);
        let expected_packed = bp.pack(&fingerprints);

        assert_eq!(*emitted_blocks.borrow(), expected_packed);
    }

    #[test]
    #[should_panic(expected = "bits_per_item must be between 1 and 64")]
    fn streaming_panics_on_zero_bits() {
        let _ = StreamingBitPacker::new(0, |_| {});
    }

    #[test]
    #[should_panic(expected = "bits_per_item must be between 1 and 64")]
    fn streaming_panics_on_too_many_bits() {
        let _ = StreamingBitPacker::new(65, |_| {});
    }

    // IterativeBitPacker tests

    #[test]
    fn iterative_matches_bitpacker_pack_8bit() {
        // Test that iterative produces the same result as batch packing
        let fingerprints: Vec<Vec<u8>> = (0..12u8).map(|n| [n; 32].to_vec()).collect();

        let bp = BitPacker(8);
        let expected_packed = bp.pack(&fingerprints);

        let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(8));
        for fp in &fingerprints {
            fingerprint_accumulator.push(fp);
        }

        assert_eq!(fingerprint_accumulator.fingerprint().bits, expected_packed);
    }

    #[test]
    fn iterative_matches_bitpacker_pack_9bit() {
        // Test with 9-bit items that span across block boundaries
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

        let bp = BitPacker(9);
        let expected_packed = bp.pack(&fingerprints);

        let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(9));
        for fp in &fingerprints {
            fingerprint_accumulator.push(fp);
        }

        assert_eq!(fingerprint_accumulator.fingerprint().bits, expected_packed);
    }

    #[test]
    fn iterative_matches_bitpacker_various_sizes() {
        // Test various bit sizes
        for bits_per_item in [1, 2, 4, 7, 8, 9, 15, 16, 17, 32, 63, 64] {
            let fingerprints: Vec<Vec<u8>> = (0..20u8).map(|n| [n; 32].to_vec()).collect();

            let bp = BitPacker(bits_per_item);
            let expected_packed = bp.pack(&fingerprints);

            let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(bits_per_item));
            for fp in &fingerprints {
                fingerprint_accumulator.push(fp);
            }

            assert_eq!(
                fingerprint_accumulator.fingerprint().bits, expected_packed,
                "Mismatch for bits_per_item={}",
                bits_per_item
            );
        }
    }

    #[test]
    fn iterative_append_with_offset() {
        // Test appending to existing packed data using with_offset
        let fingerprints_first: Vec<Vec<u8>> = (0..8u8).map(|n| [n; 32].to_vec()).collect();
        let fingerprints_second: Vec<Vec<u8>> = (8..16u8).map(|n| [n; 32].to_vec()).collect();
        let fingerprints_all: Vec<Vec<u8>> = (0..16u8).map(|n| [n; 32].to_vec()).collect();

        let bp = BitPacker(8);
        let expected_packed = bp.pack(&fingerprints_all);

        // First, pack the first batch
        let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(8));
        for fp in &fingerprints_first {
            fingerprint_accumulator.push(fp);
        }

        // Then, append the second batch using with_offset
        let fp = fingerprint_accumulator.into_fingerprint();
        let mut fingerprint_accumulator =
            FingerprintAccumulator::from(Fingerprint::from(fp.bits, fp.bits_packer, fp.len));
        for item in &fingerprints_second {
            fingerprint_accumulator.push(item);
        }

        assert_eq!(fingerprint_accumulator.fingerprint().bits, expected_packed);
    }

    #[test]
    fn iterative_append_with_offset_9bit() {
        // Test appending with 9-bit items (spans block boundaries)
        let fingerprints_first: Vec<Vec<u8>> = (0..7u8).map(|n| [n; 32].to_vec()).collect();
        let fingerprints_second: Vec<Vec<u8>> = (7..14u8).map(|n| [n; 32].to_vec()).collect();
        let fingerprints_all: Vec<Vec<u8>> = (0..14u8).map(|n| [n; 32].to_vec()).collect();

        let bp = BitPacker(9);
        let expected_packed = bp.pack(&fingerprints_all);

        // First, pack the first batch
        let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(9));
        for item in &fingerprints_first {
            fingerprint_accumulator.push(item);
        }

        // Then, append the second batch using with_offset
        let fp = fingerprint_accumulator.into_fingerprint();
        let mut fingerprint_accumulator =
            FingerprintAccumulator::from(Fingerprint::from(fp.bits, fp.bits_packer, fp.len));
        for item in &fingerprints_second {
            fingerprint_accumulator.push(item);
        }

        assert_eq!(fingerprint_accumulator.fingerprint().bits, expected_packed);
    }

    #[test]
    fn iterative_auto_grow_vec() {
        // Push 8 items = 64 bits = 1 block
        let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(8));
        for i in 0..8u8 {
            fingerprint_accumulator.push(&[i]);
        }
        assert_eq!(fingerprint_accumulator.fingerprint().bits.len(), 1);

        // Push 8 more items = 64 more bits = 2 blocks total
        let fp = fingerprint_accumulator.into_fingerprint();
        let mut fingerprint_accumulator =
            FingerprintAccumulator::from(Fingerprint::from(fp.bits, fp.bits_packer, fp.len));
        for i in 8..16u8 {
            fingerprint_accumulator.push(&[i]);
        }
        assert_eq!(fingerprint_accumulator.fingerprint().bits.len(), 2);
    }

    #[test]
    fn iterative_item_count() {
        let mut fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(8));
        assert_eq!(fingerprint_accumulator.len(), 0);

        fingerprint_accumulator.push(&[1]);
        assert_eq!(fingerprint_accumulator.len(), 1);

        fingerprint_accumulator.push(&[2]);
        assert_eq!(fingerprint_accumulator.len(), 2);

        for _ in 0..10 {
            fingerprint_accumulator.push(&[0]);
        }
        assert_eq!(fingerprint_accumulator.len(), 12);
    }

    #[test]
    fn iterative_bits_per_item_accessor() {
        let fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(9));
        assert_eq!(fingerprint_accumulator.fingerprint().bits_packer.bits_per_item(), 9);

        let fingerprint_accumulator2 = FingerprintAccumulator::new(BitPacker(16));
        assert_eq!(fingerprint_accumulator2.fingerprint().bits_packer.bits_per_item(), 16);
    }

    #[test]
    fn iterative_empty_vec() {
        // Test with no items pushed
        let fingerprint_accumulator = FingerprintAccumulator::new(BitPacker(8));
        assert_eq!(fingerprint_accumulator.fingerprint().bits.len(), 0);
    }
}
