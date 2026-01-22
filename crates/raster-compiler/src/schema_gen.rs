use raster_core::{schema::SequenceSchema, Result};

/// Generates sequence schemas from source code.
pub struct SchemaGenerator {}

impl SchemaGenerator {
    pub fn new() -> Self {
        Self {}
    }

    /// Generate a schema from a sequence definition.
    pub fn generate(&self, _source: &str) -> Result<SequenceSchema> {
        // TODO: Implement schema generation
        // - Parse sequence macro usage
        // - Extract control flow
        // - Generate schema
        todo!("Schema generation not yet implemented")
    }
}

impl Default for SchemaGenerator {
    fn default() -> Self {
        Self::new()
    }
}
