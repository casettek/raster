use raster_core::{Error, Result};

/// Validate that the provided input matches the tile's expected inputs.
pub fn encode_input(input: Option<&str>) -> Result<Vec<u8>> {
    let input_bytes = if let Some(input_json) = input {
        // Parse JSON input and serialize with postcard
        let value: serde_json::Value =
            serde_json::from_str(input_json).map_err(|e| Error::Other(format!("Failed to parse input JSON: {}", e)))?;
        postcard::to_allocvec(&value).map_err(|e| Error::Other(format!("Failed to serialize input: {}", e)))?
    } else {
        // Empty input (unit type)
        postcard::to_allocvec(&()).map_err(|e| Error::Other(format!("Failed to serialize empty input: {}", e)))?
    };
    Ok(input_bytes)
}

/// Decode tile output bytes based on the tile's return type.
pub fn decode_output(output_type: &str, output: &[u8]) -> String {
    // Try to decode based on the type hint
    match output_type.trim() {
        // String types
        "String" | "& str" | "&str" => {
            postcard::from_bytes::<String>(output)
                .map(|s| format!("\"{}\"", s))
                .unwrap_or_else(|_| format!("<{} bytes>", output.len()))
        }
        
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
        "()" => "()".to_string(),
        
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
        
        // Result types - unwrap if possible
        t if t.starts_with("Result<") || t.starts_with("Result <") => {
            // Try common inner types
            if let Ok(s) = postcard::from_bytes::<std::result::Result<String, String>>(output) {
                match s {
                    Ok(v) => format!("Ok(\"{}\")", v),
                    Err(e) => format!("Err(\"{}\")", e),
                }
            } else if let Ok(n) = postcard::from_bytes::<std::result::Result<u64, String>>(output) {
                match n {
                    Ok(v) => format!("Ok({})", v),
                    Err(e) => format!("Err(\"{}\")", e),
                }
            } else {
                format!("<Result: {} bytes>", output.len())
            }
        }
        
        // Unknown types - try serde_json::Value as fallback
        _ => {
            postcard::from_bytes::<serde_json::Value>(output)
                .map(|v| v.to_string())
                .unwrap_or_else(|_| format!("<{}: {} bytes>", output_type, output.len()))
        }
    }
}

