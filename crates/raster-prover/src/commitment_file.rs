//! Host storage for the existing Postcard commitment format.
use crate::{
    error::{BitPackerError, Result},
    trace::{
        self, Bytes, BytesHashable, CommitmentRead, FraudProofConfig, TraceCommitment, TraceRead,
        TraceTree,
    },
};
use raster_core::{
    fingerprint::{BitPacker, Fingerprint, FingerprintAccumulator},
    merkle::{to_node_hash, NodeHash},
    trace::StepRecord,
};
use serde::{
    de::{Error as _, SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

/// One computation for streamed and retained traces. Only the revealed windows
/// and an unfinished, word-aligned fingerprint batch remain in memory.
pub struct CommitmentBuilder {
    tree: TraceTree,
    config: FraudProofConfig,
    batch: FingerprintAccumulator,
    blocks: Blocks,
    head: Vec<StepRecord>,
    tail: std::collections::VecDeque<NodeHash>,
    len: usize,
}
impl CommitmentBuilder {
    pub fn new(seed: &[u8], config: FraudProofConfig) -> Self {
        Self::with_blocks(seed, config, Blocks::new())
    }
    pub fn in_memory(seed: &[u8], config: FraudProofConfig) -> Self {
        Self::with_blocks(seed, config, Blocks::memory(Vec::new()))
    }
    fn with_blocks(seed: &[u8], config: FraudProofConfig, blocks: Blocks) -> Self {
        let mut tree = TraceTree::new(1);
        tree.append(Bytes(seed.to_vec()));
        Self {
            tree,
            config,
            batch: FingerprintAccumulator::new(BitPacker(config.bits_per_item)),
            blocks,
            head: Vec::new(),
            tail: std::collections::VecDeque::new(),
            len: 0,
        }
    }
    pub fn push(&mut self, record: StepRecord) -> Result<()> {
        if self.len == u32::MAX as usize || !self.tree.append(Bytes(record.try_hash()?)) {
            return Err(BitPackerError::InvalidWindow(
                "trace tree capacity exceeded".into(),
            ));
        }
        let root = self.tree.root(0).expect("trace root");
        if self.len < self.config.window_size {
            self.head.push(record);
        }
        self.tail.push_back(to_node_hash(&root.0));
        if self.tail.len() > self.config.window_size {
            self.tail.pop_front();
        }
        self.batch.append(&root.0);
        self.len += 1;
        if self.batch.len() == 64 {
            self.flush_batch();
        }
        Ok(())
    }
    fn flush_batch(&mut self) {
        let batch = std::mem::replace(
            &mut self.batch,
            FingerprintAccumulator::new(BitPacker(self.config.bits_per_item)),
        );
        for block in batch.into_fingerprint().bits {
            self.blocks.push(block);
        }
    }
    pub fn finish(mut self) -> Result<CommitmentFile> {
        if self.len <= self.config.window_size {
            return Err(BitPackerError::InvalidWindow(
                "trace length outside commitment limits".into(),
            ));
        }
        self.flush_batch();
        let value = CommitmentFile {
            fingerprint: StoredFingerprint {
                bits_packer: BitPacker(self.config.bits_per_item),
                bits: self.blocks,
                len: self.len,
            },
            revealed_items: self.head,
            revealed_tail_roots: self.tail.into_iter().collect(),
        };
        trace::validate_commitment(&value)?;
        Ok(value)
    }
}

#[derive(Debug, Clone)]
pub struct Blocks {
    file: Option<Arc<Mutex<File>>>,
    memory: Vec<u64>,
    len: usize,
}
impl Blocks {
    fn new() -> Self {
        Self {
            file: Some(Arc::new(Mutex::new(
                tempfile::tempfile_in(crate::archive::scratch_root().expect("scratch root"))
                    .expect("create fingerprint scratch file"),
            ))),
            memory: Vec::new(),
            len: 0,
        }
    }
    fn memory(values: Vec<u64>) -> Self {
        Self {
            len: values.len(),
            memory: values,
            file: None,
        }
    }
    fn push(&mut self, value: u64) {
        if self.file.is_none() {
            self.memory.push(value);
            self.len += 1;
            return;
        }
        let mut file = self
            .file
            .as_ref()
            .expect("disk fingerprint")
            .lock()
            .expect("fingerprint lock");
        file.seek(SeekFrom::End(0)).expect("seek fingerprint");
        file.write_all(&value.to_le_bytes())
            .expect("write fingerprint (check free disk space)");
        self.len += 1;
    }
    pub fn set(&mut self, index: usize, value: u64) {
        assert!(index < self.len);
        if self.file.is_none() {
            self.memory[index] = value;
            return;
        }
        if Arc::strong_count(self.file.as_ref().unwrap()) > 1 {
            let mut copy = Self::new();
            for i in 0..self.len {
                copy.push(self.get(i));
            }
            *self = copy;
        }
        let mut file = self
            .file
            .as_ref()
            .expect("disk fingerprint")
            .lock()
            .expect("fingerprint lock");
        file.seek(SeekFrom::Start(index as u64 * 8))
            .expect("seek fingerprint");
        file.write_all(&value.to_le_bytes())
            .expect("write fingerprint");
    }
    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        (0..self.len).map(|i| self.get(i))
    }
    pub fn get(&self, index: usize) -> u64 {
        assert!(index < self.len);
        if self.file.is_none() {
            return self.memory[index];
        }
        let mut file = self
            .file
            .as_ref()
            .expect("disk fingerprint")
            .lock()
            .expect("fingerprint lock");
        file.seek(SeekFrom::Start(index as u64 * 8))
            .expect("seek fingerprint");
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes).expect("read fingerprint");
        u64::from_le_bytes(bytes)
    }
}
impl<'a> IntoIterator for &'a Blocks {
    type Item = u64;
    type IntoIter = Box<dyn Iterator<Item = u64> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}
impl Serialize for Blocks {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.len))?;
        for i in 0..self.len {
            seq.serialize_element(&self.get(i))?;
        }
        seq.end()
    }
}
impl<'de> Deserialize<'de> for Blocks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct BlockVisitor;
        impl<'de> Visitor<'de> for BlockVisitor {
            type Value = Blocks;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("fingerprint blocks")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Blocks, A::Error> {
                if seq.size_hint().is_some_and(|len| len > u32::MAX as usize) {
                    return Err(A::Error::custom("fingerprint tree capacity exceeded"));
                }
                let mut blocks = Blocks::new();
                while let Some(value) = seq.next_element()? {
                    blocks.push(value);
                }
                Ok(blocks)
            }
        }
        deserializer.deserialize_seq(BlockVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFingerprint {
    pub bits_packer: BitPacker,
    pub bits: Blocks,
    pub len: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitmentFile {
    pub fingerprint: StoredFingerprint,
    #[serde(deserialize_with = "read_window")]
    pub revealed_items: Vec<StepRecord>,
    #[serde(deserialize_with = "read_window")]
    pub revealed_tail_roots: Vec<NodeHash>,
}
fn read_window<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
) -> std::result::Result<Vec<T>, D::Error> {
    struct WindowVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for WindowVisitor<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded commitment window")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Vec<T>, A::Error> {
            let mut values = Vec::new();
            if seq
                .size_hint()
                .is_some_and(|len| len > trace::MAX_FRAUD_PROOF_WINDOW_SIZE)
            {
                return Err(A::Error::custom("commitment window exceeds capacity"));
            }
            while let Some(value) = seq.next_element()? {
                if values.len() == trace::MAX_FRAUD_PROOF_WINDOW_SIZE {
                    return Err(A::Error::custom("commitment window exceeds capacity"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    de.deserialize_seq(WindowVisitor(std::marker::PhantomData))
}
impl CommitmentRead for CommitmentFile {
    fn len(&self) -> usize {
        self.fingerprint.len
    }
    fn packer(&self) -> BitPacker {
        self.fingerprint.bits_packer
    }
    fn block_count(&self) -> usize {
        self.fingerprint.bits.len
    }
    fn block(&self, index: usize) -> u64 {
        self.fingerprint.bits.get(index)
    }
    fn head(&self) -> &[StepRecord] {
        &self.revealed_items
    }
    fn tail(&self) -> &[NodeHash] {
        &self.revealed_tail_roots
    }
}
impl StoredFingerprint {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn bits_per_item(&self) -> usize {
        self.bits_packer.bits_per_item()
    }
}
impl CommitmentFile {
    pub fn len(&self) -> usize {
        self.fingerprint.len
    }
    pub fn window_size(&self) -> usize {
        self.revealed_items.len()
    }
    pub fn validate(&self) -> Result<()> {
        trace::validate_commitment(self)
    }

    pub fn build(records: &impl TraceRead, seed: &[u8], config: FraudProofConfig) -> Result<Self> {
        let mut builder = CommitmentBuilder::new(seed, config);
        for record in records.records() {
            builder.push(record)?;
        }
        builder.finish()
    }
    pub fn into_memory(self) -> TraceCommitment {
        TraceCommitment {
            fingerprint: Fingerprint::from(
                (0..self.block_count()).map(|i| self.block(i)).collect(),
                self.packer(),
                self.len(),
            ),
            revealed_items: self.revealed_items,
            revealed_tail_roots: self.revealed_tail_roots,
        }
    }
    pub fn header(&self) -> raster_core::transition::TraceCommitmentHeader {
        trace::commitment_header(self)
    }
    pub fn fingerprint_slice_witness(
        &self,
        start: usize,
        len: usize,
    ) -> raster_core::transition::FingerprintSliceWitness {
        trace::commitment_slice_witness(self, start, len)
    }
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if self.fingerprint.bits.file.is_some() {
            crate::archive::Archive::current().flush()?;
        }
        // Publish only a completely serialized commitment.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut output = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = BufWriter::new(output.as_file_mut());
            postcard::to_io(self, &mut writer).map_err(std::io::Error::other)?;
            writer.flush()?;
        }
        output.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
    /// Read a completed artifact without opening any working-storage files.
    pub fn read_memory(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let (value, remainder): (TraceCommitment, _) =
            postcard::take_from_bytes(&bytes).map_err(std::io::Error::other)?;
        if !remainder.is_empty() {
            return Err(std::io::Error::other("trailing commitment bytes"));
        }
        let value = Self {
            fingerprint: StoredFingerprint {
                bits_packer: value.fingerprint.bits_packer,
                len: value.fingerprint.len,
                bits: Blocks::memory(value.fingerprint.bits),
            },
            revealed_items: value.revealed_items,
            revealed_tail_roots: value.revealed_tail_roots,
        };
        value.validate().map_err(std::io::Error::other)?;
        Ok(value)
    }
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let arena = bumpalo::Bump::new();
        let reader = OwnedReader {
            reader: BufReader::new(File::open(path)?),
            scratch: Vec::new(),
            arena: &arena,
        };
        let mut de = postcard::Deserializer::from_flavor(reader);
        let value = Self::deserialize(&mut de).map_err(std::io::Error::other)?;
        let mut reader = de.finalize().map_err(std::io::Error::other)?;
        if reader.read(&mut [0; 1])? != 0 {
            return Err(std::io::Error::other("trailing commitment bytes"));
        }
        trace::validate_commitment(&value).map_err(std::io::Error::other)?;
        Ok(value)
    }
}

// StepRecord contains owned strings/bytes. Reuse a temporary buffer for each
// such field instead of retaining the complete encoded commitment.
struct OwnedReader<'a, R> {
    reader: R,
    scratch: Vec<u8>,
    arena: &'a bumpalo::Bump,
}
impl<'de, R: Read + 'de> postcard::de_flavors::Flavor<'de> for OwnedReader<'de, R> {
    type Remainder = R;
    type Source = R;
    fn pop(&mut self) -> postcard::Result<u8> {
        let mut byte = [0];
        self.reader
            .read_exact(&mut byte)
            .map_err(|_| postcard::Error::DeserializeUnexpectedEnd)?;
        Ok(byte[0])
    }
    fn try_take_n(&mut self, count: usize) -> postcard::Result<&'de [u8]> {
        let bytes = self.arena.alloc_slice_fill_copy(count, 0u8);
        self.reader
            .read_exact(bytes)
            .map_err(|_| postcard::Error::DeserializeUnexpectedEnd)?;
        Ok(bytes)
    }
    fn try_take_n_temp<'a>(&'a mut self, count: usize) -> postcard::Result<&'a [u8]>
    where
        'de: 'a,
    {
        self.scratch.resize(count, 0);
        self.reader
            .read_exact(&mut self.scratch)
            .map_err(|_| postcard::Error::DeserializeUnexpectedEnd)?;
        Ok(&self.scratch)
    }
    fn finalize(self) -> postcard::Result<R> {
        Ok(self.reader)
    }
}
