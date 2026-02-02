use std::io::Write;
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use raster_core::trace::{TraceItem, TraceInputParam};
use raster_prover::bit_packer::BitPacker;
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::TraceCommitmentProducer;

use crate::tracing::subscriber::Subscriber;
pub struct CommitSubscriber<W>
where
    W: Write + Send,
{
    producer: Mutex<Option<TraceCommitmentProducer>>,
    writer: Mutex<W>,
    bit_packer: Mutex<BitPacker>,
}

impl<W: Write + Send> CommitSubscriber<W> {
    /// Creates a new execution commitment subscriber.
    ///
    /// Uses the precomputed empty trie node as the seed for the Merkle tree.
    pub fn new(bits: usize, writer: W) -> Self {
        let writer = Mutex::new(writer);
        Self {
            // TODO: seed hardcoded for now
            producer: Mutex::new(Some(TraceCommitmentProducer::new(&EMPTY_TRIE_NODES[0]))),
            writer: writer,
            bit_packer: Mutex::new(BitPacker::new(bits)),
        }
    }
}

impl<W: Write + Send> Subscriber for CommitSubscriber<W> {
    fn on_trace(
        &self,
        function_name: &str,
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
    ) {
        let item = TraceItem {
            fn_name: function_name.to_string(),
            desc: desc.map(|s| s.to_string()),
            inputs: input_params
                .iter()
                .map(|(name, ty)| TraceInputParam {
                    name: name.to_string(),
                    ty: ty.to_string(),
                })
                .collect(),
            input_data: BASE64_STANDARD.encode(input),
            output_type: output_type.map(|s| s.to_string()),
            output_data: BASE64_STANDARD.encode(output),
        };

        if let Ok(mut producer) = self.producer.lock() {
            if let Some(producer) = producer.as_mut() {
                producer.try_append(&item).unwrap();
            }
        }
    }

    fn on_complete(&self) {
        if let Ok(mut producer_guard) = self.producer.lock() {
            if let Some(producer) = producer_guard.take() {
                let trace_items_commitments = producer.finish();
                if let Ok(bit_packer) = self.bit_packer.lock() {
                    let packed = bit_packer.pack(&trace_items_commitments);

                    if let Ok(mut writer) = self.writer.lock() {
                        for block in packed {
                            writer.write_all(&block.to_le_bytes()).unwrap();
                        }
                    }
                }
            }
        }
    }
}