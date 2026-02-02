use std::sync::OnceLock;

/// A trait for receiving trace events.
pub trait Subscriber: Send + Sync {
    /// Called when a function completes, with serialized input/output bytes and metadata.
    ///
    /// # Arguments
    /// - `function_name` - Name of the function being traced
    /// - `desc` - Optional human-readable description of the tile
    /// - `input_params` - Slice of (name, type) tuples for each input parameter
    /// - `output_type` - Optional return type as a string
    /// - `input` - Serialized input bytes (postcard-encoded)
    /// - `output` - Serialized output bytes (postcard-encoded)
    fn on_trace(
        &self,
        function_name: &str,
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
    );

    fn on_complete(&self);
}

// TODO: consider adding linkme here
/// The global subscriber instance.
pub(crate) static GLOBAL_SUBSCRIBER: OnceLock<Box<dyn Subscriber>> = OnceLock::new();

pub mod json;
pub mod verify;
pub mod commit;