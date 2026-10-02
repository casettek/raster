//! Every value shape's payload re-derives the root it was encoded with.
//!
//! [`raster_core::tree`] is the one encoder: storage, drafts and (under
//! `tile-io-structural-roots`) tile guests take a value's raster root from it.
//! A stored object's root is also recomputed from its payload bytes alone —
//! [`payload_structural_root`], by the chain verifier, the reader and the
//! selection checks — and that parser spells the same hash rule a second time.
//! The two must agree on every shape, or a correctly encoded object fails its
//! own checks. See `docs/proposals/tile-io-structural-roots.md` §Step 0.

use std::collections::BTreeMap;

use raster_core::collections::{Block, Bytes, BytesPage, List};
use raster_core::input::payload_structural_root;
use raster_core::tree::{subtree_payload_and_root, tree_value_from_serialize, typed_value_from_tree};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// `Some((encoder root, parser root))`, or `None` if the encoder refuses.
fn encoder_roots<T: Serialize>(value: &T) -> Option<([u8; 32], Option<[u8; 32]>)> {
    let tree = tree_value_from_serialize(value).ok()?;
    let (payload, root) = subtree_payload_and_root(&tree).expect("a serialized tree encodes");
    Some((root, payload_structural_root(&payload)))
}

/// [`encoder_roots`], and asserts the decoder inverts the encoder: decoding the tree back into
/// `T` and re-encoding it reproduces the same tree. A draft is completed into
/// its typed value this way, in the tile replay as well as on the host.
fn roots<T: Serialize + DeserializeOwned>(value: &T) -> Option<([u8; 32], Option<[u8; 32]>)> {
    let tree = tree_value_from_serialize(value).ok()?;
    let decoded: T = typed_value_from_tree(&tree).expect("a serialized tree decodes");
    assert_eq!(
        tree_value_from_serialize(&decoded).expect("a decoded value re-encodes"),
        tree,
        "decode is not the inverse of encode for {}",
        core::any::type_name::<T>()
    );
    let (payload, root) = subtree_payload_and_root(&tree).expect("a serialized tree encodes");
    Some((root, payload_structural_root(&payload)))
}

#[derive(Serialize, Deserialize)]
struct Unit;
#[derive(Serialize, Deserialize)]
struct Newtype(u32);
#[derive(Serialize, Deserialize)]
struct TupleStruct(u8, String);
#[derive(Serialize, Deserialize)]
struct Plain {
    count: u64,
    name: String,
    flag: bool,
}
#[derive(Serialize, Deserialize)]
struct Nested {
    inner: Plain,
    tag: i16,
}
#[derive(Serialize, Deserialize)]
struct WithList {
    title: String,
    lines: List<String>,
}
#[derive(Serialize, Deserialize)]
struct WithBlock {
    title: String,
    window: Block<u32>,
}
#[derive(Serialize, Deserialize)]
struct WithListOfStructs {
    rows: List<Plain>,
}
#[derive(Serialize, Deserialize)]
struct WithOption {
    maybe: Option<u64>,
    nested: Option<Plain>,
}
#[derive(Serialize, Deserialize)]
enum Shape {
    Empty,
    Radius(u32),
    Pair(u8, u8),
    Rect { width: u16, height: u16 },
}
#[derive(Serialize, Deserialize)]
struct WithEnums {
    first: Shape,
    rest: Vec<Shape>,
}
#[derive(Serialize, Deserialize)]
struct WithMap {
    index: BTreeMap<String, u32>,
}
#[derive(Serialize, Deserialize)]
struct WithNestedLists {
    grid: List<List<u8>>,
    wrapped: Vec<List<u16>>,
    maybe: Option<List<String>>,
    pair: (List<u8>, u8),
}
#[derive(Serialize, Deserialize)]
enum Carrier {
    Lines(List<String>),
    Named { rows: List<Plain>, tail: u8 },
}
#[derive(Serialize, Deserialize)]
struct WithStructMap {
    rows: BTreeMap<u32, Plain>,
}
#[derive(Serialize, Deserialize)]
struct WithBytes {
    head: u8,
    body: Bytes<4>,
}

/// A value that serializes through `serialize_bytes`, as `serde_bytes` does.
struct RawBytes<'a>(&'a [u8]);
impl Serialize for RawBytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}
fn serde_bytes_like(bytes: &[u8]) -> RawBytes<'_> {
    RawBytes(bytes)
}

fn plain(count: u64) -> Plain {
    Plain {
        count,
        name: format!("row-{count}"),
        flag: count % 2 == 0,
    }
}

#[test]
fn every_payload_rederives_its_root() {
    // (name, must encode, roots)
    let mut cases: Vec<(&str, bool, Option<([u8; 32], Option<[u8; 32]>)>)> = Vec::new();
    macro_rules! case {
        ($name:expr, $value:expr) => {
            cases.push(($name, true, roots(&$value)))
        };
    }
    macro_rules! refused {
        ($name:expr, $value:expr) => {
            cases.push(($name, false, encoder_roots(&$value)))
        };
    }

    case!("unit ()", ());
    case!("unit struct", Unit);
    case!("bool", true);
    case!("u8", 7u8);
    case!("u16", 700u16);
    case!("u32", 70_000u32);
    case!("u64", u64::MAX);
    case!("i8", -7i8);
    case!("i16", -700i16);
    case!("i32", -70_000i32);
    case!("i64", i64::MIN);
    case!("string", String::from("hello"));
    case!("empty string", String::new());
    case!("newtype struct", Newtype(3));
    case!("tuple", (1u8, String::from("x")));
    case!("tuple struct", TupleStruct(2, String::from("y")));
    case!("option none", Option::<u64>::None);
    case!("option some", Some(5u64));
    case!("vec", vec![1u32, 2, 3]);
    case!("empty vec", Vec::<u32>::new());
    case!("nested vec", vec![vec![1u8], vec![], vec![2, 3]]);
    case!("struct", plain(1));
    case!(
        "nested struct",
        Nested {
            inner: plain(2),
            tag: -1
        }
    );
    case!(
        "struct with option",
        WithOption {
            maybe: Some(9),
            nested: None
        }
    );
    case!("enum unit", Shape::Empty);
    case!("enum newtype", Shape::Radius(4));
    case!("enum tuple", Shape::Pair(1, 2));
    case!(
        "enum struct",
        Shape::Rect {
            width: 3,
            height: 4
        }
    );
    case!(
        "struct with enums",
        WithEnums {
            first: Shape::Radius(1),
            rest: vec![
                Shape::Empty,
                Shape::Rect {
                    width: 1,
                    height: 2
                }
            ],
        }
    );
    case!(
        "map",
        WithMap {
            index: BTreeMap::from([(String::from("a"), 1), (String::from("b"), 2)]),
        }
    );
    case!(
        "top-level List",
        List::from(vec![String::from("a"), String::from("b")])
    );
    case!("empty List", List::<u32>::new());
    case!(
        "struct with List field",
        WithList {
            title: String::from("t"),
            lines: List::from(vec![String::from("l1"), String::from("l2")]),
        }
    );
    case!(
        "struct with empty List field",
        WithList {
            title: String::from("t"),
            lines: List::new(),
        }
    );
    case!(
        "struct with List of structs",
        WithListOfStructs {
            rows: List::from(vec![plain(1), plain(2), plain(3)]),
        }
    );
    case!("top-level Block", Block::__from_selection(vec![1u32, 2]));
    case!(
        "struct with Block field",
        WithBlock {
            title: String::from("t"),
            window: Block::__from_selection(vec![5u32, 6, 7]),
        }
    );
    case!("BytesPage", BytesPage::__from_parts(0, 0, vec![1, 2, 3, 4]));
    case!(
        "Bytes<4>",
        Bytes::<4>::paged(vec![1u8, 2, 3, 4, 5, 6]).expect("pages")
    );
    case!(
        "empty Bytes<4>",
        Bytes::<4>::paged(Vec::new()).expect("pages")
    );
    case!(
        "struct with Bytes field",
        WithBytes {
            head: 1,
            body: Bytes::<4>::paged((0..=20u8).collect::<Vec<u8>>()).expect("pages"),
        }
    );
    case!(
        "List nested in every container",
        WithNestedLists {
            grid: List::from(vec![List::from(vec![1u8, 2]), List::new()]),
            wrapped: vec![List::from(vec![3u16]), List::new()],
            maybe: Some(List::from(vec![String::from("m")])),
            pair: (List::from(vec![9u8]), 4),
        }
    );
    case!(
        "List in enum newtype",
        Carrier::Lines(List::from(vec![String::from("x")]))
    );
    case!(
        "List in enum struct",
        Carrier::Named {
            rows: List::from(vec![plain(4)]),
            tail: 7,
        }
    );
    case!(
        "map of structs",
        WithStructMap {
            rows: BTreeMap::from([(1, plain(1)), (2, plain(2))]),
        }
    );
    case!("long string", "x".repeat(70_000));
    case!("large vec", (0..5_000u32).collect::<Vec<_>>());
    case!("large List", List::from((0..5_000u64).collect::<Vec<_>>()));
    case!(
        "large Bytes<4>",
        Bytes::<4>::paged(vec![7u8; 4097]).expect("pages")
    );

    // Neither encoder has a node for these; both must refuse.
    refused!("u128", 1u128);
    refused!("i128", -1i128);
    refused!("f32", 1.5f32);
    refused!("f64", 1.5f64);
    refused!("char", 'c');
    refused!("bytes via serialize_bytes", serde_bytes_like(&[1, 2, 3]));

    let failures: Vec<String> = cases
        .iter()
        .filter(|(_, encodes, roots)| match roots {
            Some((encoded, parsed)) => !(*encodes && *parsed == Some(*encoded)),
            None => *encodes,
        })
        .map(|(name, encodes, roots)| {
            let expected = if *encodes {
                "parser root = encoder root"
            } else {
                "refused"
            };
            format!("  {name} (expected {expected}): {roots:02x?}")
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases fail:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
