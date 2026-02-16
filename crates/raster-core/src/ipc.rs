//! Inter-process communication protocol for raster CLI and user programs.
//!
//! This module defines the prefixes and helper functions for IPC communication
//! between the raster CLI and user programs executed via subprocess.

use crate::trace::{AuditResult, TraceItem};

/// Prefix for base64-encoded tile output.
pub const OUTPUT_PREFIX: &str = "RASTER_OUTPUT:";

/// Prefix for JSON-encoded trace items.
pub const TRACE_PREFIX: &str = "RASTER_TRACE:";

/// Prefix for JSON-encoded audit results.
pub const AUDIT_PREFIX: &str = "RASTER_AUDIT:";

// ============================================================================
// Parser functions
// ============================================================================

/// Parse a tile output line, returning the base64 payload if the line matches.
///
/// # Example
/// ```
/// use raster_core::ipc::parse_output;
///
/// let line = "RASTER_OUTPUT:SGVsbG8gV29ybGQ=";
/// assert_eq!(parse_output(line), Some("SGVsbG8gV29ybGQ="));
///
/// let other = "Some other line";
/// assert_eq!(parse_output(other), None);
/// ```
pub fn parse_output(line: &str) -> Option<&str> {
    line.strip_prefix(OUTPUT_PREFIX)
}

/// Parse a trace item line, returning the deserialized TraceItem if the line matches.
///
/// Returns `None` if the line doesn't have the trace prefix.
/// Returns `Some(Err(...))` if the line has the prefix but JSON parsing fails.
pub fn parse_trace(line: &str) -> Option<Result<TraceItem, serde_json::Error>> {
    line.strip_prefix(TRACE_PREFIX)
        .map(serde_json::from_str)
}

/// Parse a trace item line as a raw JSON Value (for display purposes).
///
/// Returns `None` if the line doesn't have the trace prefix.
/// Returns `Some(Err(...))` if the line has the prefix but JSON parsing fails.
pub fn parse_trace_value(line: &str) -> Option<Result<serde_json::Value, serde_json::Error>> {
    line.strip_prefix(TRACE_PREFIX)
        .map(serde_json::from_str)
}

/// Parse an audit result line, returning the deserialized AuditResult if the line matches.
///
/// Returns `None` if the line doesn't have the audit prefix.
/// Returns `Some(Err(...))` if the line has the prefix but JSON parsing fails.
pub fn parse_audit(line: &str) -> Option<Result<AuditResult, serde_json::Error>> {
    line.strip_prefix(AUDIT_PREFIX)
        .map(serde_json::from_str)
}

// ============================================================================
// Emitter functions
// ============================================================================

/// Emit a tile output line to stdout with the appropriate prefix.
///
/// The `base64_data` should be the base64-encoded output bytes.
pub fn emit_output(base64_data: &str) {
    std::println!("{}{}", OUTPUT_PREFIX, base64_data);
}

/// Emit a trace item to stdout with the appropriate prefix.
///
/// The trace item is serialized as JSON.
pub fn emit_trace(item: &TraceItem) {
    if let Ok(json) = serde_json::to_string(item) {
        std::println!("{}{}", TRACE_PREFIX, json);
    }
}

/// Emit an audit result to stdout with the appropriate prefix.
///
/// The audit result is serialized as JSON.
pub fn emit_audit(result: &AuditResult) {
    if let Ok(json) = serde_json::to_string(result) {
        std::println!("{}{}", AUDIT_PREFIX, json);
    }
}

// ============================================================================
// Unified message parsing
// ============================================================================

/// A parsed IPC message from program output.
#[derive(Debug)]
pub enum IpcMessage<'a> {
    /// Base64-encoded tile output.
    Output(&'a str),
    /// Parsed trace item.
    Trace(TraceItem),
    /// Parsed audit result.
    Audit(AuditResult),
    /// Regular program output (not an IPC message).
    Unknown(&'a str),
}

/// Parse a line of program output into an IPC message.
///
/// This function attempts to parse the line as each known IPC message type
/// in order, returning the first match. If no prefix matches, returns
/// `IpcMessage::Unknown` with the original line.
///
/// # Example
/// ```
/// use raster_core::ipc::{parse_line, IpcMessage};
///
/// let line = "Hello, world!";
/// match parse_line(line) {
///     IpcMessage::Unknown(s) => assert_eq!(s, "Hello, world!"),
///     _ => panic!("Expected Unknown"),
/// }
/// ```
pub fn parse_line(line: &str) -> IpcMessage<'_> {
    if let Some(data) = parse_output(line) {
        return IpcMessage::Output(data);
    }

    if let Some(result) = parse_trace(line) {
        if let Ok(item) = result {
            return IpcMessage::Trace(item);
        }
    }

    if let Some(result) = parse_audit(line) {
        if let Ok(audit) = result {
            return IpcMessage::Audit(audit);
        }
    }

    IpcMessage::Unknown(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_output() {
        assert_eq!(parse_output("RASTER_OUTPUT:abc123"), Some("abc123"));
        assert_eq!(parse_output("RASTER_OUTPUT:"), Some(""));
        assert_eq!(parse_output("other line"), None);
        assert_eq!(parse_output("RASTER_TRACE:{}"), None);
    }

    #[test]
    fn test_parse_line_unknown() {
        let line = "Regular program output";
        match parse_line(line) {
            IpcMessage::Unknown(s) => assert_eq!(s, line),
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_parse_line_output() {
        let line = "RASTER_OUTPUT:SGVsbG8=";
        match parse_line(line) {
            IpcMessage::Output(data) => assert_eq!(data, "SGVsbG8="),
            _ => panic!("Expected Output"),
        }
    }
}
