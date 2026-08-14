//! Walk a project's AST types into a `SchemaNode` so `InterfaceDecl.schema_hash`
//! can be filled without asking the author to write the hash in `Raster.toml`.

use std::collections::HashMap;

use raster_core::collections::bytes_schema;
use raster_core::draft::schema_hash;
use raster_core::input::{SchemaField, SchemaNode, Selectable};
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
