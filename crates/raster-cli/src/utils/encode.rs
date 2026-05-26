use raster_backend::ExecutionFailure;
use raster_core::{Error, Result, TileOutputEnvelope};

/// Validate that the provided input matches the tile's expected inputs.
pub fn encode_input(input: Option<&str>) -> Result<Vec<u8>> {
    let input_bytes = if let Some(input_json) = input {
        // Parse JSON input and serialize with postcard
        let value: serde_json::Value = serde_json::from_str(input_json)
            .map_err(|e| Error::Other(format!("Failed to parse input JSON: {}", e)))?;
        postcard::to_allocvec(&value)
            .map_err(|e| Error::Other(format!("Failed to serialize input: {}", e)))?
    } else {
        // Empty input (unit type)
        postcard::to_allocvec(&())
            .map_err(|e| Error::Other(format!("Failed to serialize empty input: {}", e)))?
    };
    Ok(input_bytes)
}

/// Decode tile output bytes based on the tile's return type.
pub fn decode_output(output_type: &str, output: &[u8]) -> String {
    // Try to decode based on the type hint
    match output_type.trim() {
        // String types
        "String" | "& str" | "&str" => postcard::from_bytes::<String>(output)
            .map(|s| format!("\"{}\"", s))
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),

        // Unsigned integers
        "u8" => postcard::from_bytes::<u8>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "u16" => postcard::from_bytes::<u16>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "u32" => postcard::from_bytes::<u32>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "u64" => postcard::from_bytes::<u64>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "usize" => postcard::from_bytes::<usize>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),

        // Signed integers
        "i8" => postcard::from_bytes::<i8>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "i16" => postcard::from_bytes::<i16>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "i32" => postcard::from_bytes::<i32>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "i64" => postcard::from_bytes::<i64>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),

        // Floats
        "f32" => postcard::from_bytes::<f32>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),
        "f64" => postcard::from_bytes::<f64>(output)
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),

        // Boolean
        "bool" => postcard::from_bytes::<bool>(output)
            .map(|b| b.to_string())
            .unwrap_or_else(|_| format!("<{} bytes>", output.len())),

        // Unit type
        "()" => postcard::from_bytes::<()>(output)
            .map(|_| "()".to_string())
            .unwrap_or_else(|_| "()".to_string()),

        // Vec types - try to decode as JSON value for display
        t if t.starts_with("Vec<") || t.starts_with("Vec <") => {
            // Try as Vec<u8> first (common case)
            if t.contains("u8") {
                postcard::from_bytes::<Vec<u8>>(output)
                    .map(|v| format!("{:?}", v))
                    .unwrap_or_else(|_| format!("<{} bytes>", output.len()))
            } else {
                // Try as JSON value for generic display
                postcard::from_bytes::<serde_json::Value>(output)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| format!("<{} bytes>", output.len()))
            }
        }

        // Unknown types - try serde_json::Value as fallback
        _ => postcard::from_bytes::<serde_json::Value>(output)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| format!("<{}: {} bytes>", output_type, output.len())),
    }
}

fn normalize_type_name(output_type: &str) -> String {
    output_type.chars().filter(|c| !c.is_whitespace()).collect()
}

fn extract_result_ok_type(output_type: &str) -> Option<String> {
    let normalized = normalize_type_name(output_type);
    let marker = "Result<";
    let result_start = normalized.find(marker)?;
    let inner = normalized.get(result_start + marker.len()..normalized.len() - 1)?;

    let mut depth = 0usize;
    for (idx, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some(inner[..idx].to_string()),
            _ => {}
        }
    }

    None
}

pub fn decode_execution_output(
    output_type: &str,
    output: &[u8],
) -> std::result::Result<String, ExecutionFailure<String>> {
    let envelope = postcard::from_bytes::<TileOutputEnvelope>(output).map_err(|e| {
        ExecutionFailure::Runtime(Error::Serialization(format!(
            "Failed to decode tile output envelope '{}': {}",
            output_type, e
        )))
    })?;

    match envelope {
        TileOutputEnvelope::Success(bytes) => {
            let success_type =
                extract_result_ok_type(output_type).unwrap_or_else(|| output_type.to_string());
            Ok(decode_output(&success_type, &bytes))
        }
        TileOutputEnvelope::UserError { display, .. } => Err(ExecutionFailure::User(display)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_execution_output_reports_user_error() {
        let output = postcard::to_allocvec(&TileOutputEnvelope::UserError {
            bytes: postcard::to_allocvec(&"denied".to_string()).unwrap(),
            display: "denied".to_string(),
        })
        .unwrap();

        match decode_execution_output("Result<(), Error>", &output) {
            Err(ExecutionFailure::User(err)) => assert_eq!(err, "denied"),
            other => panic!("expected user error, got {:?}", other),
        }
    }

    #[test]
    fn decode_execution_output_reports_success_value() {
        let output =
            postcard::to_allocvec(&TileOutputEnvelope::Success(postcard::to_allocvec(&42u64).unwrap()))
                .unwrap();

        let decoded = decode_execution_output("Result<u64, Error>", &output).unwrap();
        assert_eq!(decoded, "42");
    }
}
