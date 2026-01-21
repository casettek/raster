use raster_compiler::tile::Tile;
use anyhow::{anyhow, Context, Result};


/// Validate that the provided input matches the tile's expected inputs.
pub fn validate_tile_input(tile: &Tile, input: Option<&str>) -> Result<()> {
    let expected_inputs = &tile.function.inputs;
    let input_count = expected_inputs.len();

    match (input_count, input) {
        // No inputs expected, none provided - OK
        (0, None) => Ok(()),
        
        // No inputs expected, but input provided
        (0, Some(_)) => Err(anyhow!(
            "Tile '{}' takes no arguments, but --input was provided.\n\
             Signature: {}",
            tile.function.name,
            tile.function.signature
        )),
        
        // Inputs expected, none provided
        (n, None) => Err(anyhow!(
            "Tile '{}' requires {} argument(s), but no --input provided.\n\
             Signature: {}\n\
             Expected types: {}",
            tile.function.name,
            n,
            tile.function.signature,
            expected_inputs.join(", ")
        )),
        
        // Both present - validate structure
        (n, Some(input_json)) => {
            let value: serde_json::Value = serde_json::from_str(input_json)
                .context("Failed to parse input JSON")?;
            
            if n == 1 {
                // Single argument - validate it's not an array (unless type is array)
                validate_single_input(&tile.function.name, &expected_inputs[0], &value)
            } else {
                // Multiple arguments - expect array with correct length
                validate_multiple_inputs(&tile.function.name, expected_inputs, &value)
            }
        }
    }
}

fn validate_single_input(tile_name: &str, expected_type: &str, value: &serde_json::Value) -> Result<()> {
    // Basic type checking based on JSON value type
    let type_mismatch = match (expected_type, value) {
        // String types
        ("String" | "&str" | "& str", serde_json::Value::String(_)) => false,
        ("String" | "&str" | "& str", _) => true,
        
        // Numeric types
        ("u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize", 
         serde_json::Value::Number(_)) => false,
        ("u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize", _) => true,
        
        // Boolean
        ("bool", serde_json::Value::Bool(_)) => false,
        ("bool", _) => true,
        
        // Arrays/Vecs
        (t, serde_json::Value::Array(_)) if t.starts_with("Vec<") || t.starts_with("Vec <") => false,
        
        // Objects/structs - can't easily validate, allow any object
        (_, serde_json::Value::Object(_)) => false,
        
        // Unknown types - don't fail, let runtime handle it
        _ => false,
    };
    
    if type_mismatch {
        Err(anyhow!(
            "Tile '{}' expects input of type '{}', but got {}\n\
             Hint: Use proper JSON format, e.g., '\"hello\"' for strings, 42 for numbers",
            tile_name,
            expected_type,
            json_type_name(value)
        ))
    } else {
        Ok(())
    }
}

fn validate_multiple_inputs(tile_name: &str, expected_types: &[String], value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Array(arr) => {
            if arr.len() != expected_types.len() {
                Err(anyhow!(
                    "Tile '{}' expects {} argument(s), but got {} in array.\n\
                     Expected types: ({})\n\
                     Hint: Use JSON array format, e.g., '[\"hello\", 42]'",
                    tile_name,
                    expected_types.len(),
                    arr.len(),
                    expected_types.join(", ")
                ))
            } else {
                // Optionally validate each element
                for (i, (expected, actual)) in expected_types.iter().zip(arr.iter()).enumerate() {
                    if let Err(e) = validate_single_input(tile_name, expected, actual) {
                        return Err(anyhow!("Argument {} invalid: {}", i + 1, e));
                    }
                }
                Ok(())
            }
        }
        _ => Err(anyhow!(
            "Tile '{}' expects {} arguments, provide them as a JSON array.\n\
             Expected types: ({})\n\
             Example: --input '[\"hello\", 42]'",
            tile_name,
            expected_types.len(),
            expected_types.join(", ")
        )),
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Decode tile output bytes based on the tile's return type.
pub fn decode_tile_output(tile: &Tile, output: &[u8]) -> String {
    let output_type = tile.function.output.as_deref().unwrap_or("()");
    
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
            if let Ok(s) = postcard::from_bytes::<Result<String, String>>(output) {
                match s {
                    Ok(v) => format!("Ok(\"{}\")", v),
                    Err(e) => format!("Err(\"{}\")", e),
                }
            } else if let Ok(n) = postcard::from_bytes::<Result<u64, String>>(output) {
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

