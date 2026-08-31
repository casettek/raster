//! Reading a raster artifact back into a structured value.
//!
//! The inverse of [`crate::input::write_raster_files`]. Every raster payload is
//! one format — a program's `output.bin`, an external input's `*.rastered`, a
//! chain stage's artifact — so one reader serves all of them.
//!
//! This is not a new decoder. `parse_leaf_value` and the index walk it wraps
//! already run on every `select!`; this module is the public face of that walk
//! plus truncation limits and an integrity report. Keeping the same leaf parser
//! is the point: a viewer that disagrees with the encoder is worse than no
//! viewer.
//!
//! See `docs/proposals/artifact-inspection.md`.

use std::fs;
use std::path::Path;

use raster_core::input::payload_structural_root;
use raster_core::{Error, Result};

use crate::input::{raster_value_from_node, RasterValue};
use crate::raster_index::RasterIndex;

/// Bounds on what a single read will materialize.
///
/// A parameter rather than a constant because the same walk serves a terminal
/// (truncate hard) and `--format json` redirected to a file (truncate loosely).
/// A `Bytes<P>` page is 256 KiB by construction and a list has no bound at all,
/// so a reader without limits is a reader that will one day print a gigabyte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    /// Bytes kept from a single `String` or `BytesPage` leaf.
    pub max_bytes_per_leaf: usize,
    /// Elements kept from a single list.
    pub max_list_elements: usize,
    /// Nesting depth before a subtree is elided.
    pub max_depth: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_bytes_per_leaf: 256,
            max_list_elements: 64,
            max_depth: 32,
        }
    }
}

impl ReadLimits {
    /// Limits large enough that nothing realistic truncates — for `--format
    /// json` into a file, where the consumer is `jq` and not a terminal.
    pub fn unbounded() -> Self {
        Self {
            max_bytes_per_leaf: usize::MAX,
            max_list_elements: usize::MAX,
            max_depth: usize::MAX,
        }
    }
}

/// A decoded artifact: the value, and what the bytes say about their own
/// integrity.
///
/// The roots are reported rather than enforced. `read_raster_artifact` returns
/// a value even when they disagree, because a corrupt artifact is exactly the
/// one you most want to look at — the caller decides how loud to be.
#[derive(Debug, Clone)]
pub struct RasterArtifact {
    pub value: RasterValue,
    /// Structural root recomputed from the payload bytes alone.
    pub structural_root: String,
    /// Root commitment recorded in the `.rindex`.
    pub index_root: String,
}

impl RasterArtifact {
    /// Whether the payload's own root matches the one the index claims.
    ///
    /// A mismatch means payload and index are not the same artifact — a
    /// swapped, truncated or edited file.
    pub fn roots_agree(&self) -> bool {
        self.structural_root == self.index_root
    }
}

/// Decode an artifact from its payload and index.
pub fn read_raster_value(
    data_path: &Path,
    index_path: &Path,
    limits: &ReadLimits,
) -> Result<RasterValue> {
    read_raster_artifact(data_path, index_path, limits).map(|artifact| artifact.value)
}

/// Decode an artifact and report its commitment roots.
pub fn read_raster_artifact(
    data_path: &Path,
    index_path: &Path,
    limits: &ReadLimits,
) -> Result<RasterArtifact> {
    let data = read_artifact_file(data_path, "payload")?;
    let index_bytes = read_artifact_file(index_path, "index")?;
    read_raster_artifact_from_bytes(&data, &index_bytes, limits)
}

/// Decode from bytes already in hand — the same walk, without the file system.
pub fn read_raster_artifact_from_bytes(
    data: &[u8],
    index_bytes: &[u8],
    limits: &ReadLimits,
) -> Result<RasterArtifact> {
    let index = RasterIndex::from_bytes(index_bytes)?;
    let value = raster_value_from_node(&index, data, index.root_node, limits)?;

    // Free: the bytes are already in hand and `payload_structural_root` is
    // public. Recomputing here is what lets a caller say whether what it is
    // about to render is what was committed.
    let structural_root = payload_structural_root(data)
        .map(hex_string)
        .ok_or_else(|| Error::Serialization("Not a well-formed raster payload".into()))?;

    Ok(RasterArtifact {
        value,
        structural_root,
        index_root: index.root_commitment_hex(),
    })
}

fn read_artifact_file(path: &Path, what: &str) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| {
        Error::Other(format!(
            "failed to read raster {} '{}': {}",
            what,
            path.display(),
            e
        ))
    })
}

fn hex_string(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{encode_raster_value, RasterValue};
    use raster_core::collections::{Bytes, List};
    use serde::Serialize;
    use std::collections::BTreeMap;

    /// Encode a value the way a program's authorized output is encoded, then
    /// read it back — the round trip the whole module exists to close.
    fn round_trip<T: Serialize>(value: &T, limits: &ReadLimits) -> RasterArtifact {
        let (data, index, _commitment) = encode_raster_value(value).expect("encode");
        read_raster_artifact_from_bytes(&data, &index, limits).expect("read back")
    }

    fn int(value: i128, ty: &'static str) -> RasterValue {
        RasterValue::Int { value, ty }
    }

    fn text(value: &str) -> RasterValue {
        RasterValue::Str {
            value: value.into(),
            truncated: false,
        }
    }

    #[derive(Serialize)]
    struct Report {
        title: String,
        lines: Vec<String>,
        total: u64,
    }

    #[test]
    fn round_trips_a_struct_of_string_list_and_int() {
        let artifact = round_trip(
            &Report {
                title: "Pipeline report for sensor-A".into(),
                lines: vec!["count   : 6".into(), "sum     : 353".into()],
                total: 353,
            },
            &ReadLimits::default(),
        );

        assert_eq!(
            artifact.value,
            RasterValue::Struct(vec![
                ("title".into(), text("Pipeline report for sensor-A")),
                (
                    "lines".into(),
                    RasterValue::List {
                        len: 2,
                        elements: vec![text("count   : 6"), text("sum     : 353")],
                        truncated: false,
                    }
                ),
                ("total".into(), int(353, "u64")),
            ])
        );
        // The payload's own root and the index's agree on an untouched artifact.
        assert!(artifact.roots_agree());
    }

    #[derive(Serialize)]
    struct EveryWidth {
        a: u8,
        b: u16,
        c: u32,
        d: u64,
        e: i8,
        f: i16,
        g: i32,
        h: i64,
        i: bool,
        j: (),
    }

    #[test]
    fn every_scalar_width_keeps_its_type_name() {
        let artifact = round_trip(
            &EveryWidth {
                a: 1,
                b: 2,
                c: 3,
                d: 4,
                e: -1,
                f: -2,
                g: -3,
                h: -4,
                i: true,
                j: (),
            },
            &ReadLimits::default(),
        );
        assert_eq!(
            artifact.value,
            RasterValue::Struct(vec![
                ("a".into(), int(1, "u8")),
                ("b".into(), int(2, "u16")),
                ("c".into(), int(3, "u32")),
                ("d".into(), int(4, "u64")),
                ("e".into(), int(-1, "i8")),
                ("f".into(), int(-2, "i16")),
                ("g".into(), int(-3, "i32")),
                ("h".into(), int(-4, "i64")),
                ("i".into(), RasterValue::Bool(true)),
                ("j".into(), RasterValue::Unit),
            ])
        );
    }

    #[derive(Serialize)]
    enum Shape {
        Empty,
        Wrapped(u32),
        Pair(u32, u32),
        Named { side: u32 },
    }

    #[test]
    fn every_enum_form_is_named() {
        // Unlike a struct, an enum records its variant in the index — the
        // asymmetry §4.1 of the proposal documents.
        let limits = ReadLimits::default();
        assert_eq!(
            round_trip(&Shape::Empty, &limits).value,
            RasterValue::Enum {
                variant: "Empty".into(),
                payload: None,
            }
        );
        assert_eq!(
            round_trip(&Shape::Wrapped(7), &limits).value,
            RasterValue::Enum {
                variant: "Wrapped".into(),
                payload: Some(Box::new(int(7, "u32"))),
            }
        );
        assert_eq!(
            round_trip(&Shape::Pair(1, 2), &limits).value,
            RasterValue::Enum {
                variant: "Pair".into(),
                payload: Some(Box::new(RasterValue::List {
                    len: 2,
                    elements: vec![int(1, "u32"), int(2, "u32")],
                    truncated: false,
                })),
            }
        );
        assert_eq!(
            round_trip(&Shape::Named { side: 3 }, &limits).value,
            RasterValue::Enum {
                variant: "Named".into(),
                payload: Some(Box::new(RasterValue::Struct(vec![(
                    "side".into(),
                    int(3, "u32")
                )]))),
            }
        );
    }

    #[test]
    fn reads_a_map() {
        let mut map = BTreeMap::new();
        map.insert(1u32, "one".to_string());
        map.insert(2u32, "two".to_string());

        assert_eq!(
            round_trip(&map, &ReadLimits::default()).value,
            RasterValue::Map {
                len: 2,
                entries: vec![
                    (int(1, "u32"), text("one")),
                    (int(2, "u32"), text("two")),
                ],
                truncated: false,
            }
        );
    }

    #[test]
    fn reads_a_list_handle() {
        // `List<T>` encodes as a `0x09` handle rather than a plain `0x02`
        // list; the index-driven walk reconstructs the elements either way.
        let value: List<u32> = List::from(vec![10u32, 20, 30]);
        assert_eq!(
            round_trip(&value, &ReadLimits::default()).value,
            RasterValue::List {
                len: 3,
                elements: vec![int(10, "u32"), int(20, "u32"), int(30, "u32")],
                truncated: false,
            }
        );
    }

    #[test]
    fn reads_bytes_pages_and_bounds_them() {
        // 8 bytes over 4-byte pages: two full pages, so both exceed a 2-byte
        // limit. A short trailing page would be under the limit and would not
        // be truncated, which is correct but tests nothing.
        let bytes: Bytes<4> = Bytes::paged(vec![0xAAu8; 8]).expect("paged");
        let limits = ReadLimits {
            max_bytes_per_leaf: 2,
            ..ReadLimits::default()
        };
        let RasterValue::Struct(fields) = round_trip(&bytes, &limits).value else {
            panic!("expected a struct");
        };
        let pages = fields
            .iter()
            .find(|(name, _)| name == "pages")
            .map(|(_, value)| value)
            .expect("pages field");
        let RasterValue::List { len, elements, .. } = pages else {
            panic!("expected a list of pages");
        };
        assert_eq!(*len, 2, "8 bytes over 4-byte pages is 2 pages");

        // Every page is truncated to the limit and says so — a 256 KiB page
        // must never render in full.
        for page in elements {
            let RasterValue::Bytes {
                data, truncated, ..
            } = page
            else {
                panic!("expected a bytes page");
            };
            assert_eq!(data.len(), 2);
            assert!(truncated);
        }
    }

    #[test]
    fn list_truncation_reports_the_true_length() {
        let value: Vec<u32> = (0..100).collect();
        let limits = ReadLimits {
            max_list_elements: 3,
            ..ReadLimits::default()
        };
        let RasterValue::List {
            len,
            elements,
            truncated,
        } = round_trip(&value, &limits).value
        else {
            panic!("expected a list");
        };
        // The count is the artifact's, not the rendering's — otherwise a
        // truncated view would silently misreport the data.
        assert_eq!(len, 100);
        assert_eq!(elements.len(), 3);
        assert!(truncated);
    }

    #[test]
    fn string_truncation_lands_on_a_char_boundary() {
        // Four-byte codepoints against a limit that falls mid-character: the
        // cut must not produce invalid UTF-8.
        let value = "🦀🦀🦀".to_string();
        let limits = ReadLimits {
            max_bytes_per_leaf: 6,
            ..ReadLimits::default()
        };
        let RasterValue::Str { value, truncated } = round_trip(&value, &limits).value else {
            panic!("expected a string");
        };
        assert_eq!(value, "🦀");
        assert!(truncated);
    }

    #[derive(Serialize)]
    struct Outer {
        inner: Inner,
    }

    #[derive(Serialize)]
    struct Inner {
        deep: u32,
    }

    #[test]
    fn depth_limit_elides_rather_than_descending() {
        let limits = ReadLimits {
            max_depth: 1,
            ..ReadLimits::default()
        };
        assert_eq!(
            round_trip(&Outer { inner: Inner { deep: 1 } }, &limits).value,
            RasterValue::Struct(vec![("inner".into(), RasterValue::Elided)])
        );
    }

    #[test]
    fn a_flipped_payload_byte_is_reported_not_hidden() {
        let value = Report {
            title: "Pipeline report for sensor-A".into(),
            lines: vec!["count   : 6".into()],
            total: 353,
        };
        let (mut data, index, _) = encode_raster_value(&value).expect("encode");

        // Flip a bit inside the title's UTF-8, which changes the payload's
        // structural root while leaving the index — and so the shape — intact.
        let position = data
            .windows(7)
            .position(|window| window == b"sensor-")
            .expect("title bytes present");
        data[position] ^= 0x01;

        let artifact = read_raster_artifact_from_bytes(&data, &index, &ReadLimits::default())
            .expect("a corrupt artifact still renders — that is the point");
        assert!(
            !artifact.roots_agree(),
            "a flipped byte must surface as a commitment mismatch"
        );
    }

    #[test]
    fn a_malformed_payload_is_refused() {
        let value = Report {
            title: "t".into(),
            lines: Vec::new(),
            total: 1,
        };
        let (_data, index, _) = encode_raster_value(&value).expect("encode");
        assert!(read_raster_artifact_from_bytes(b"not a raster payload", &index, &ReadLimits::default()).is_err());
    }

    #[test]
    fn unbounded_limits_keep_everything() {
        let value: Vec<u32> = (0..1000).collect();
        let RasterValue::List {
            elements,
            truncated,
            ..
        } = round_trip(&value, &ReadLimits::unbounded()).value
        else {
            panic!("expected a list");
        };
        assert_eq!(elements.len(), 1000);
        assert!(!truncated);
    }
}
