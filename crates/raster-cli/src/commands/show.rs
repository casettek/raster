//! `cargo raster show` — read a raster artifact back into a printable value.
//!
//! One command over every raster payload, because they are one format: a
//! program's `output.bin`, an external input's `*.rastered`, a chain stage's
//! artifact. The decode lives in `raster-runtime`; this module is rendering and
//! argument handling only.
//!
//! The same renderer backs `run --show-output` / `chain run --show-output`
//! (see [`show_run_output`]), which is what keeps the flag sugar over this
//! command rather than a second way to read an artifact.
//!
//! See `docs/proposals/artifact-inspection.md`.

use std::path::{Path, PathBuf};

use raster_core::{Error, Result};
use raster_runtime::{read_raster_artifact, RasterArtifact, RasterValue, ReadLimits};

use crate::ShowFormat;

/// Rendering width for a truncated commitment.
const SHORT_HASH_CHARS: usize = 16;

/// `cargo raster show <artifact>`.
pub fn show(
    artifact: &str,
    index: Option<&str>,
    format: ShowFormat,
    max_bytes: Option<usize>,
    max_list: Option<usize>,
    depth: Option<usize>,
) -> Result<()> {
    let data_path = PathBuf::from(artifact);
    let index_path = match index {
        Some(path) => PathBuf::from(path),
        None => inferred_index_path(&data_path),
    };

    if !data_path.is_file() {
        return Err(Error::Other(format!(
            "no raster payload at '{}'",
            data_path.display()
        )));
    }
    // The structural fallback that would let a bare payload be read is deferred
    // (`artifact-inspection.md` §2), so a missing index is a refusal — and it
    // names the path it looked for, because the usual cause is that the index
    // sits somewhere else under a name we did not guess.
    if !index_path.is_file() {
        return Err(Error::Other(format!(
            "no raster index at '{}'\n  \
             `show` needs the artifact's `.rindex` to type its leaves. Pass --index <path> if it \
             lives elsewhere.\n  \
             Reading a payload without its index (structural mode) is not implemented.",
            index_path.display()
        )));
    }

    let limits = resolve_limits(format, max_bytes, max_list, depth);
    let artifact = read_raster_artifact(&data_path, &index_path, &limits)?;

    match format {
        ShowFormat::Text => {
            print!("{}", render_text(&artifact.value));
            println!();
            print_integrity(&artifact, &data_path, "");
        }
        ShowFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&render_json(&artifact.value)).map_err(|e| {
                Error::Other(format!("failed to encode artifact as JSON: {e}"))
            })?
        ),
    }
    Ok(())
}

/// Render the `output.bin` a run just produced, for `--show-output`.
///
/// Deliberately the same decode and the same renderer as [`show`] — the flag
/// supplies paths it already knows and nothing else, so the two surfaces cannot
/// drift apart in what they say a value is.
///
/// `Ok(false)` means the program returned unit and wrote no artifact, which is
/// not an error: a terminal stage may legitimately produce nothing.
pub fn show_run_output(run_dir: &Path, label: Option<&str>) -> Result<bool> {
    let data_path = run_dir.join("output.bin");
    let index_path = run_dir.join("output.rindex");
    if !data_path.is_file() || !index_path.is_file() {
        return Ok(false);
    }

    let artifact = read_raster_artifact(&data_path, &index_path, &ReadLimits::default())?;
    println!();
    match label {
        // A distinct header from the `Output:` block that already carries
        // `raster::println!` lines — the two must not read as one.
        Some(name) => println!("Program output value ({name}):"),
        None => println!("Program output value:"),
    }
    for line in render_text(&artifact.value).lines() {
        println!("  {line}");
    }
    print_integrity(&artifact, &data_path, "  ");
    Ok(true)
}

/// Text limits bound a terminal; JSON limits bound a pipe, so they default off.
fn resolve_limits(
    format: ShowFormat,
    max_bytes: Option<usize>,
    max_list: Option<usize>,
    depth: Option<usize>,
) -> ReadLimits {
    let base = match format {
        ShowFormat::Text => ReadLimits::default(),
        ShowFormat::Json => ReadLimits::unbounded(),
    };
    ReadLimits {
        max_bytes_per_leaf: max_bytes.unwrap_or(base.max_bytes_per_leaf),
        max_list_elements: max_list.unwrap_or(base.max_list_elements),
        max_depth: depth.unwrap_or(base.max_depth),
    }
}

/// Mirrors the chain runner's own default (`chain.rs`): the payload path with
/// its extension replaced by `rindex`, so `show output.bin` just works.
fn inferred_index_path(data_path: &Path) -> PathBuf {
    data_path.with_extension("rindex")
}

/// State what was read and whether it is what was committed.
///
/// Reports rather than enforces — exit stays 0 on a mismatch. A corrupt
/// artifact is the one you most want rendered, and a viewer that refuses to
/// show you the bytes is not helping you find out why they are wrong.
fn print_integrity(artifact: &RasterArtifact, data_path: &Path, indent: &str) {
    let root = &artifact.structural_root;
    if artifact.roots_agree() {
        println!("{indent}commitment {}…  ✓ matches .rindex", short(root));
        return;
    }
    println!(
        "{indent}commitment {}…  ✗ MISMATCH — payload and index are not the same artifact",
        short(root)
    );
    println!("{indent}  payload {}", root);
    println!("{indent}  index   {}", artifact.index_root);
    println!(
        "{indent}  '{}' has been edited, truncated or swapped since its index was written.",
        data_path.display()
    );
}

fn short(hash: &str) -> String {
    hash.chars().take(SHORT_HASH_CHARS).collect()
}

/// Render a value as an indented tree.
///
/// `Debug`-shaped, with one thing `Debug` has that this cannot: a struct name.
/// `RasterNodeKind::Struct` records field names and no type name, so structs
/// are bracketed anonymously while enum variants — which the index *does*
/// record — are named. See `artifact-inspection.md` §4.1.
pub fn render_text(value: &RasterValue) -> String {
    let mut out = String::new();
    write_text(value, 0, &mut out);
    out.push('\n');
    out
}

fn write_text(value: &RasterValue, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    match value {
        RasterValue::Unit => out.push_str("()"),
        RasterValue::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        RasterValue::Int { value, ty } => out.push_str(&format!("{value}{ty}")),
        RasterValue::Str { value, truncated } => {
            out.push_str(&format!("{:?}", value));
            if *truncated {
                out.push_str(" … truncated");
            }
        }
        RasterValue::Bytes {
            index,
            offset,
            len,
            data,
            truncated,
        } => {
            out.push_str(&format!(
                "<page {index} @{offset} {len}B> {}",
                hex_preview(data)
            ));
            if *truncated {
                out.push_str(&format!(" … {} more bytes", len.saturating_sub(data.len() as u64)));
            }
        }
        RasterValue::Struct(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (name, field) in fields {
                out.push_str(&inner_pad);
                out.push_str(name);
                out.push_str(": ");
                write_text(field, indent + 1, out);
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
        RasterValue::List {
            len,
            elements,
            truncated,
        } => {
            if elements.is_empty() && !*truncated {
                out.push_str(&format!("[{len}] []"));
                return;
            }
            out.push_str(&format!("[{len}] [\n"));
            for element in elements {
                out.push_str(&inner_pad);
                write_text(element, indent + 1, out);
                out.push('\n');
            }
            if *truncated {
                out.push_str(&inner_pad);
                out.push_str(&format!(
                    "… {} more elements\n",
                    len.saturating_sub(elements.len() as u64)
                ));
            }
            out.push_str(&pad);
            out.push(']');
        }
        RasterValue::Map {
            len,
            entries,
            truncated,
        } => {
            if entries.is_empty() && !*truncated {
                out.push_str(&format!("{{{len}}} {{}}"));
                return;
            }
            out.push_str(&format!("{{{len}}} {{\n"));
            for (key, entry) in entries {
                out.push_str(&inner_pad);
                write_text(key, indent + 1, out);
                out.push_str(" => ");
                write_text(entry, indent + 1, out);
                out.push('\n');
            }
            if *truncated {
                out.push_str(&inner_pad);
                out.push_str(&format!(
                    "… {} more entries\n",
                    len.saturating_sub(entries.len() as u64)
                ));
            }
            out.push_str(&pad);
            out.push('}');
        }
        RasterValue::Enum { variant, payload } => {
            out.push_str(variant);
            if let Some(payload) = payload {
                out.push(' ');
                write_text(payload, indent, out);
            }
        }
        RasterValue::Elided => out.push_str("… (depth limit)"),
    }
}

fn hex_preview(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<Vec<_>>()
        .join("")
}

/// Render a value as JSON, so it composes with `jq` and with tests.
///
/// Truncation is represented, never silent: a truncated list becomes an object
/// carrying its true `len`, and a whole list otherwise stays a plain array.
pub fn render_json(value: &RasterValue) -> serde_json::Value {
    use serde_json::{json, Value};
    match value {
        RasterValue::Unit => Value::Null,
        RasterValue::Bool(v) => json!(v),
        // Widening to `i128` makes every encoder width representable, but JSON
        // has no i128. A `u64` above `i64::MAX` would wrap to a negative number
        // if it went out as `i64`, so pick the lane that fits — one of the two
        // always does, because the encoder has no width wider than 64 bits.
        RasterValue::Int { value, ty } => {
            let _ = ty;
            match u64::try_from(*value) {
                Ok(unsigned) => json!(unsigned),
                Err(_) => json!(i64::try_from(*value).unwrap_or_default()),
            }
        }
        RasterValue::Str { value, truncated } => {
            if *truncated {
                json!({ "value": value, "truncated": true })
            } else {
                json!(value)
            }
        }
        RasterValue::Bytes {
            index,
            offset,
            len,
            data,
            truncated,
        } => json!({
            "page": index,
            "offset": offset,
            "len": len,
            "hex": hex_preview(data),
            "truncated": truncated,
        }),
        RasterValue::Struct(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), render_json(field)))
                .collect(),
        ),
        RasterValue::List {
            len,
            elements,
            truncated,
        } => {
            let items: Vec<Value> = elements.iter().map(render_json).collect();
            if *truncated {
                json!({ "len": len, "elements": items, "truncated": true })
            } else {
                Value::Array(items)
            }
        }
        RasterValue::Map {
            len,
            entries,
            truncated,
        } => {
            let items: Vec<Value> = entries
                .iter()
                .map(|(key, value)| json!([render_json(key), render_json(value)]))
                .collect();
            if *truncated {
                json!({ "len": len, "entries": items, "truncated": true })
            } else {
                Value::Array(items)
            }
        }
        RasterValue::Enum { variant, payload } => match payload {
            Some(payload) => json!({ variant.clone(): render_json(payload) }),
            None => json!(variant),
        },
        RasterValue::Elided => json!({ "elided": "depth limit" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn int(value: i128, ty: &'static str) -> RasterValue {
        RasterValue::Int { value, ty }
    }

    fn text(value: &str) -> RasterValue {
        RasterValue::Str {
            value: value.into(),
            truncated: false,
        }
    }

    #[test]
    fn text_renders_a_struct_anonymously_and_names_a_variant() {
        // The §4.1 asymmetry, in the output rather than in the prose.
        let value = RasterValue::Struct(vec![
            ("total".into(), int(353, "u64")),
            (
                "shape".into(),
                RasterValue::Enum {
                    variant: "Named".into(),
                    payload: Some(Box::new(RasterValue::Struct(vec![(
                        "side".into(),
                        int(3, "u32"),
                    )]))),
                },
            ),
        ]);
        assert_eq!(
            render_text(&value),
            "{\n  total: 353u64\n  shape: Named {\n    side: 3u32\n  }\n}\n"
        );
    }

    #[test]
    fn text_states_list_truncation_with_the_true_count() {
        let value = RasterValue::List {
            len: 940,
            elements: vec![int(1, "u8"), int(2, "u8")],
            truncated: true,
        };
        let rendered = render_text(&value);
        assert!(rendered.contains("[940] ["), "true length: {rendered}");
        assert!(
            rendered.contains("… 938 more elements"),
            "elision must be visible and counted: {rendered}"
        );
    }

    #[test]
    fn json_keys_an_enum_by_its_variant() {
        assert_eq!(
            render_json(&RasterValue::Enum {
                variant: "Wrapped".into(),
                payload: Some(Box::new(int(7, "u32"))),
            }),
            json!({ "Wrapped": 7 })
        );
        assert_eq!(
            render_json(&RasterValue::Enum {
                variant: "Empty".into(),
                payload: None,
            }),
            json!("Empty")
        );
    }

    #[test]
    fn json_keeps_a_u64_above_i64_max_positive() {
        // Widened to i128 internally; going out as i64 would wrap it negative.
        let value = int(u64::MAX as i128, "u64");
        assert_eq!(render_json(&value), json!(u64::MAX));
    }

    #[test]
    fn json_keeps_negative_values_negative() {
        assert_eq!(render_json(&int(-4, "i64")), json!(-4));
    }

    #[test]
    fn json_marks_truncation_rather_than_dropping_it() {
        // An untruncated list stays a plain array so `jq` sees a list …
        assert_eq!(
            render_json(&RasterValue::List {
                len: 1,
                elements: vec![text("a")],
                truncated: false,
            }),
            json!(["a"])
        );
        // … and a truncated one changes shape, so it cannot be mistaken for
        // the whole thing.
        assert_eq!(
            render_json(&RasterValue::List {
                len: 9,
                elements: vec![text("a")],
                truncated: true,
            }),
            json!({ "len": 9, "elements": ["a"], "truncated": true })
        );
    }
}
