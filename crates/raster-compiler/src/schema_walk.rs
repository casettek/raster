//! Walk a project's AST types into a `SchemaNode` so `InterfaceDecl.schema_hash`
//! can be filled without asking the author to write the hash in `Raster.toml`.

use std::collections::HashMap;

use raster_core::collections::bytes_schema;
use raster_core::draft::schema_hash;
use raster_core::input::{SchemaField, SchemaNode, Selectable, SelectorSegment};
use raster_core::program::ProgramManifest;
use raster_core::{Error, Result};
use syn::{GenericArgument, Item, PathArguments, Type};

use crate::ast::{ProjectAst, StructAstItem, StructFieldAst};

/// Resolve `type_path` (a `main` argument or return type) against the structs
/// collected from `src/`.
pub fn schema_of_type(type_path: &str, structs: &[StructAstItem]) -> Result<SchemaNode> {
    let by_name: HashMap<&str, &StructAstItem> = structs
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect();
    resolve_ast_type(&parse_ast_type(type_path)?, &by_name)
}

/// The page size of the `Bytes<N>` that `path` reaches from a value of type
/// `type_path`, following named fields through `structs`. `None` when the walk
/// leaves what the source declares: an unknown struct or field, a selector
/// segment that is not a field, index or range, or an end that is not `Bytes`.
///
/// This is the same `N` the `select!` macro divides a literal byte offset by
/// (`page_index_for_field` / `page_index_for_region`), read from the source
/// instead of from rustc's types.
pub(crate) fn bytes_page_size_at(
    type_path: &str,
    path: &[SelectorSegment],
    structs: &[StructAstItem],
) -> Option<u64> {
    let mut ty: Type = syn::parse_str(type_path).ok()?;
    for segment in path {
        ty = match segment {
            SelectorSegment::Field(name) => {
                let struct_name = type_head(peel_views(&ty))?;
                let item = structs.iter().find(|item| item.name == struct_name)?;
                let field = item.fields.iter().find(|field| field.name == *name)?;
                syn::parse_str(&field.ty).ok()?
            }
            // An element of a `List<T>` / `Block<T>` is a `T`.
            SelectorSegment::Index(_) => {
                let inner = peel_views(&ty);
                match type_head(inner)?.as_str() {
                    "List" | "Block" => first_syn_type_arg(inner)?.clone(),
                    _ => return None,
                }
            }
            // A range of a list is still a list of the same elements.
            SelectorSegment::Range { .. } => ty,
            _ => return None,
        };
    }
    let ty = peel_views(&ty);
    if type_head(ty)? != "Bytes" {
        return None;
    }
    let Type::Path(type_path) = ty else {
        return None;
    };
    const_u64_arg(&type_path.path.segments.last()?.arguments).ok()
}

/// Look through the wrappers a binding's declared type can carry around the
/// stored value: `AuthRef<T>`, `Result<T>`, a recur handle, a draft.
fn peel_views(ty: &Type) -> &Type {
    const VIEWS: [&str; 8] = [
        "AuthRef",
        "Result",
        "Draft",
        "Box",
        "RecurInput",
        "RecurOutput",
        "RecurSequenceInput",
        "RecurSequenceOutput",
    ];
    match type_head(ty) {
        Some(head) if VIEWS.contains(&head.as_str()) => match first_syn_type_arg(ty) {
            Some(inner) => peel_views(inner),
            None => ty,
        },
        _ => ty,
    }
}

/// The last path segment's name: `Model` for `crate::input::Model`.
fn type_head(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(type_path) => Some(type_path.path.segments.last()?.ident.to_string()),
        Type::Reference(reference) => type_head(&reference.elem),
        Type::Paren(paren) => type_head(&paren.elem),
        _ => None,
    }
}

/// The first type argument of the last path segment: `T` in `List<T>`.
fn first_syn_type_arg(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let PathArguments::AngleBracketed(args) = &type_path.path.segments.last()?.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

/// Fill every `InterfaceDecl.schema_hash` from the project's AST.
pub fn fill_schema_hashes(ast: &ProjectAst, manifest: &mut ProgramManifest) -> Result<()> {
    for decl in manifest.inputs.values_mut() {
        if decl.type_path.is_empty() {
            continue;
        }
        let schema = schema_of_type(&decl.type_path, &ast.structs)?;
        decl.schema_hash = schema_hash(&schema);
    }
    if let Some(output) = manifest.output.as_mut() {
        if !output.type_path.is_empty() {
            let schema = schema_of_type(&output.type_path, &ast.structs)?;
            output.schema_hash = schema_hash(&schema);
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum AstType {
    Named(String),
    List(Box<AstType>),
    Block(Box<AstType>),
    Bytes(u64),
}

fn parse_ast_type(type_path: &str) -> Result<AstType> {
    let ty: Type = syn::parse_str(type_path).map_err(|e| {
        Error::Other(format!(
            "failed to parse interface type `{type_path}`: {e}"
        ))
    })?;
    ast_type_from_syn(&ty)
}

fn ast_type_from_syn(ty: &Type) -> Result<AstType> {
    let Type::Path(type_path) = ty else {
        return Err(Error::Other(format!(
            "unsupported interface type `{}`",
            quote::quote!(#ty)
        )));
    };
    let segment = type_path.path.segments.last().ok_or_else(|| {
        Error::Other("empty type path in interface declaration".into())
    })?;
    let name = segment.ident.to_string();
    match name.as_str() {
        "List" => Ok(AstType::List(Box::new(first_type_arg(&segment.arguments)?))),
        "Block" => Ok(AstType::Block(Box::new(first_type_arg(&segment.arguments)?))),
        "Bytes" => Ok(AstType::Bytes(const_u64_arg(&segment.arguments)?)),
        // Fallible `main` / tile outputs are `Result<T>`; identity hashes `T`.
        "Result" => first_type_arg(&segment.arguments),
        _ => Ok(AstType::Named(name)),
    }
}

fn first_type_arg(args: &PathArguments) -> Result<AstType> {
    let PathArguments::AngleBracketed(args) = args else {
        return Err(Error::Other("expected a type argument".into()));
    };
    for arg in &args.args {
        if let GenericArgument::Type(ty) = arg {
            return ast_type_from_syn(ty);
        }
    }
    Err(Error::Other("expected a type argument".into()))
}

fn const_u64_arg(args: &PathArguments) -> Result<u64> {
    let PathArguments::AngleBracketed(args) = args else {
        return Err(Error::Other("Bytes requires a page-size const generic".into()));
    };
    for arg in &args.args {
        if let GenericArgument::Const(syn::Expr::Lit(expr_lit)) = arg {
            if let syn::Lit::Int(int) = &expr_lit.lit {
                return int.base10_parse().map_err(|e| {
                    Error::Other(format!("invalid Bytes page size: {e}"))
                });
            }
        }
    }
    Err(Error::Other("Bytes requires a page-size const generic".into()))
}

fn resolve_ast_type(
    ty: &AstType,
    structs: &HashMap<&str, &StructAstItem>,
) -> Result<SchemaNode> {
    match ty {
        AstType::Bytes(page_size) => Ok(bytes_schema(*page_size)),
        AstType::List(inner) => Ok(SchemaNode::List {
            type_name: "List".into(),
            element: Box::new(resolve_ast_type(inner, structs)?),
        }),
        AstType::Block(inner) => Ok(SchemaNode::List {
            type_name: "Block".into(),
            element: Box::new(resolve_ast_type(inner, structs)?),
        }),
        AstType::Named(name) => {
            if let Some(schema) = leaf_schema(name) {
                return Ok(schema);
            }
            let item = structs.get(name.as_str()).ok_or_else(|| {
                Error::Other(format!(
                    "unknown interface type `{name}`: no matching struct in src/"
                ))
            })?;
            let mut fields = Vec::with_capacity(item.fields.len());
            for field in &item.fields {
                fields.push(SchemaField::new(
                    field.name.clone(),
                    field.name.clone(),
                    resolve_ast_type(&parse_ast_type(&field.ty)?, structs)?,
                ));
            }
            Ok(SchemaNode::Struct {
                type_name: item.name.clone(),
                fields,
            })
        }
    }
}

fn leaf_schema(name: &str) -> Option<SchemaNode> {
    Some(match name {
        "bool" => bool::schema(),
        "String" => String::schema(),
        "usize" => usize::schema(),
        "u64" => u64::schema(),
        "u32" => u32::schema(),
        "u16" => u16::schema(),
        "u8" => u8::schema(),
        "i64" => i64::schema(),
        "i32" => i32::schema(),
        "i16" => i16::schema(),
        "i8" => i8::schema(),
        "BytesPage" => SchemaNode::Leaf {
            type_name: "BytesPage".into(),
        },
        _ => return None,
    })
}

/// Parse structs out of a syn file's items (used by `ProjectAst`).
pub fn collect_structs(items: &[Item]) -> Vec<StructAstItem> {
    let mut structs = Vec::new();
    collect_structs_from_items(items, &mut structs);
    structs
}

fn collect_structs_from_items(items: &[Item], out: &mut Vec<StructAstItem>) {
    for item in items {
        match item {
            Item::Struct(item_struct) => {
                let syn::Fields::Named(fields) = &item_struct.fields else {
                    continue;
                };
                let mut parsed = Vec::new();
                for field in &fields.named {
                    let Some(ident) = &field.ident else {
                        parsed.clear();
                        break;
                    };
                    let ty = &field.ty;
                    parsed.push(StructFieldAst {
                        name: ident.to_string(),
                        ty: quote::quote!(#ty).to_string(),
                    });
                }
                if !parsed.is_empty() || fields.named.is_empty() {
                    out.push(StructAstItem {
                        name: item_struct.ident.to_string(),
                        fields: parsed,
                    });
                }
            }
            Item::Mod(item_mod) => {
                if let Some((_, items)) = &item_mod.content {
                    collect_structs_from_items(items, out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::input::Schema;

    #[test]
    fn bytes_type_matches_selectable_hash() {
        let schema = schema_of_type("Bytes<4>", &[]).unwrap();
        assert_eq!(schema_hash(&schema), raster_core::Bytes::<4>::schema_hash());
    }

    /// `leaf_schema` is a hand-maintained copy of every `Selectable` impl, so
    /// each entry needs a test pinning it against the real one. This is the
    /// argument for deleting the walk rather than growing it — see
    /// `paged-bytes.md` §12.4.
    /// The page size is read through struct fields, list elements and the
    /// wrappers a binding's type carries.
    #[test]
    fn bytes_page_size_is_read_through_the_selected_path() {
        let structs = vec![
            StructAstItem {
                name: "Layer".into(),
                fields: vec![StructFieldAst { name: "weights".into(), ty: "Bytes < 196_608 >".into() }],
            },
            StructAstItem {
                name: "Model".into(),
                fields: vec![
                    StructFieldAst { name: "layers".into(), ty: "List < Layer >".into() },
                    StructFieldAst { name: "name".into(), ty: "String".into() },
                ],
            },
        ];
        let field = |name: &str| SelectorSegment::Field(name.into());
        let path = [field("layers"), SelectorSegment::Index(2), field("weights")];
        assert_eq!(bytes_page_size_at("Model", &path, &structs), Some(196_608));
        assert_eq!(bytes_page_size_at("AuthRef<Model>", &path, &structs), Some(196_608));
        assert_eq!(bytes_page_size_at("Bytes<4>", &[], &structs), Some(4));
        assert_eq!(bytes_page_size_at("Model", &[field("name")], &structs), None);
        assert_eq!(bytes_page_size_at("Model", &[field("missing")], &structs), None);
        assert_eq!(bytes_page_size_at("Unknown", &[field("weights")], &structs), None);
    }

    #[test]
    fn bytes_page_type_matches_selectable_hash() {
        let schema = schema_of_type("BytesPage", &[]).unwrap();
        assert_eq!(schema_hash(&schema), raster_core::BytesPage::schema_hash());
    }

    /// The realistic reason the arm exists: a stage whose `main` consumes pages
    /// produced upstream. Without it this is a hard build failure, not a wrong
    /// hash.
    #[test]
    fn list_of_bytes_pages_resolves() {
        let schema = schema_of_type("List<BytesPage>", &[]).unwrap();
        assert_eq!(
            schema_hash(&schema),
            raster_core::List::<raster_core::BytesPage>::schema_hash()
        );
    }

    #[test]
    fn struct_with_bytes_matches_hand_built_schema() {
        let structs = vec![StructAstItem {
            name: "ModelFile".into(),
            fields: vec![StructFieldAst {
                name: "weights".into(),
                ty: "Bytes<4>".into(),
            }],
        }];
        let schema = schema_of_type("ModelFile", &structs).unwrap();
        match &schema {
            SchemaNode::Struct { type_name, fields } => {
                assert_eq!(type_name, "ModelFile");
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name, "weights");
                assert_eq!(*fields[0].schema, bytes_schema(4));
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }
}
