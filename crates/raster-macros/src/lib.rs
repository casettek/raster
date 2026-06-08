//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and generates its ABI wrapper
//! - `#[sequence]` - Declares tile ordering and control flow. When the function is named `main`,
//!   it is the program entry point and gets init and finish automatically.

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, Attribute, Expr, ExprField, ExprIndex, FnArg, GenericArgument, ItemFn,
    LitInt, Pat, Path, PathArguments, ReturnType, Token, Type,
};

#[derive(Clone)]
struct ParamInfo {
    ident: syn::Ident,
    ty: Type,
}

fn extract_params(input: &ItemFn) -> Vec<ParamInfo> {
    input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(pat_ident) => Some(ParamInfo {
                    ident: pat_ident.ident.clone(),
                    ty: (*pat_type.ty).clone(),
                }),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect()
}

fn parse_schema_tag(attrs: &[Attribute]) -> Option<u32> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("schema") {
            return None;
        }

        let mut tag = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                let value = meta.value()?;
                let lit: syn::LitInt = value.parse()?;
                tag = lit.base10_parse().ok();
            }
            Ok(())
        });

        tag
    })
}

fn rewrite_into_auth_value_args(sig: &mut syn::Signature) {
    for arg in sig.inputs.iter_mut() {
        if let FnArg::Typed(pat_type) = arg {
            let ty = &pat_type.ty;
            pat_type.ty = syn::parse_quote!(impl ::raster::IntoAuthValue<#ty>);
        }
    }
}

fn external_info_ident(param: &ParamInfo) -> syn::Ident {
    format_ident!("__raster_external_info_{}", param.ident)
}

fn internal_info_ident(param: &ParamInfo) -> syn::Ident {
    format_ident!("__raster_internal_info_{}", param.ident)
}

fn sequence_root_ident(index: usize) -> syn::Ident {
    format_ident!("__RasterRoot{}", index)
}

fn sequence_arg_ident(index: usize) -> syn::Ident {
    format_ident!("__RasterArg{}", index)
}

fn gen_auth_value_materialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_params(input);
    let resolutions: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let value_ty = &param.ty;
            let external_info_ident = external_info_ident(param);
            let internal_info_ident = internal_info_ident(param);
            quote! {
                let __raster_auth_value = ::raster::into_auth_value::<#value_ty, _>(#name)
                    .unwrap_or_else(|e| panic!("Failed to materialize auth value for argument '{}': {}", stringify!(#name), e));
                let #external_info_ident = __raster_auth_value.as_external().cloned();
                let #internal_info_ident = __raster_auth_value.as_internal().cloned();
                let #name: #value_ty = __raster_auth_value.into_inner();
            }
        })
        .collect();

    quote! {
        #(#resolutions)*
    }
}

fn gen_sequence_input_serialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_params(input);

    let trace_defs: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let trace_value_ident = format_ident!("__raster_input_value_{}", name);
            let external_info_ident = external_info_ident(param);
            let internal_info_ident = internal_info_ident(param);
            quote! {
                let __raster_auth_trace = ::raster::auth_ref_trace(&#name)
                    .unwrap_or_else(|e| panic!("Failed to trace sequence argument '{}': {}", stringify!(#name), e));
                let #trace_value_ident = __raster_auth_trace.value;
                let #external_info_ident = __raster_auth_trace.external;
                let #internal_info_ident = __raster_auth_trace.internal;
            }
        })
        .collect();

    let input_arg_defs: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let name_str = name.to_string();
            let ty = &param.ty;
            let ty_str = ty.to_token_stream().to_string();
            quote! {
                ::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(#name_str),
                    ty: ::raster::alloc::string::String::from(#ty_str),
                }
            }
        })
        .collect();

    let trace_values: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let trace_value_ident = format_ident!("__raster_input_value_{}", name);
            quote! { #trace_value_ident }
        })
        .collect();

    let input_bytes = if params.is_empty() {
        quote! {
            let __raster_input_bytes: ::raster::alloc::vec::Vec<u8> = ::raster::alloc::vec::Vec::new();
        }
    } else {
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(
                &::raster::alloc::vec![#(#trace_values.clone()),*]
            ).unwrap_or_default();
        }
    };

    let external_binding_entries: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let external_info_ident = external_info_ident(param);
            quote! {
                if let ::core::option::Option::Some(__raster_external_info) = #external_info_ident.clone() {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        __raster_external_info,
                    );
                }
            }
        })
        .collect();

    let internal_binding_entries: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let internal_info_ident = internal_info_ident(param);
            quote! {
                if let ::core::option::Option::Some(__raster_internal_info) = #internal_info_ident.clone() {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        __raster_internal_info,
                    );
                }
            }
        })
        .collect();

    quote! {
        #(#trace_defs)*

        let __raster_input_args: ::raster::alloc::vec::Vec<::raster::core::trace::FnInputArg> = ::raster::alloc::vec![
            #(#input_arg_defs),*
        ];

        #input_bytes

        let mut __raster_external = ::raster::alloc::collections::BTreeMap::new();
        #(#external_binding_entries)*
        let mut __raster_internal = ::raster::alloc::collections::BTreeMap::new();
        #(#internal_binding_entries)*

        let __raster_input = ::core::option::Option::Some(
            ::raster::core::trace::FnInput {
                data: __raster_input_bytes,
                values: ::raster::alloc::vec![#(#trace_values.clone()),*],
                args: __raster_input_args,
                external: __raster_external,
                internal: __raster_internal,
            }
        );
    }
}

fn is_string_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(type_path) if {
        type_path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "String")
    })
}

fn is_bare_result_path(path: &Path) -> bool {
    path.leading_colon.is_none()
        && path.segments.len() == 1
        && path
            .segments
            .first()
            .is_some_and(|segment| segment.ident == "Result")
}

fn path_segments_match(path: &Path, segments: &[&str]) -> bool {
    path.segments.len() == segments.len()
        && path
            .segments
            .iter()
            .zip(segments.iter())
            .all(|(segment, expected)| segment.ident == *expected)
}

fn is_raster_exec_result_path(path: &Path) -> bool {
    path_segments_match(path, &["raster", "exec", "Result"])
}

fn is_std_result_path(path: &Path) -> bool {
    path_segments_match(path, &["std", "result", "Result"])
        || path_segments_match(path, &["core", "result", "Result"])
}

fn fallible_result_message() -> &'static str {
    "Fallible Raster functions must return bare `Result<T>` / `Result<T, String>`, `raster::exec::Result<T>`, or `std::result::Result<T, String>` / `core::result::Result<T, String>`."
}

fn validate_protocol_return_type(input: &ItemFn) {
    let ReturnType::Type(_, ty) = &input.sig.output else {
        return;
    };

    let Type::Path(type_path) = &**ty else {
        return;
    };

    let Some(segment) = type_path.path.segments.last() else {
        return;
    };

    if segment.ident != "Result" {
        return;
    }

    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        panic!("{}", fallible_result_message());
    };

    let type_args: Vec<_> = args
        .args
        .iter()
        .filter_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect();

    if is_bare_result_path(&type_path.path) {
        match type_args.as_slice() {
            [_ok_type] => {}
            [_ok_type, err_type] if is_string_type(err_type) => {}
            _ => panic!("{}", fallible_result_message()),
        }
        return;
    }

    if is_raster_exec_result_path(&type_path.path) {
        match type_args.as_slice() {
            [_ok_type] => {}
            _ => panic!("{}", fallible_result_message()),
        }
        return;
    }

    if is_std_result_path(&type_path.path) {
        match type_args.as_slice() {
            [_ok_type, err_type] if is_string_type(err_type) => {}
            _ => panic!("{}", fallible_result_message()),
        }
        return;
    }

    panic!("{}", fallible_result_message());
}

#[derive(Clone)]
enum ProtocolReturnKind {
    Unit,
    Value(Type),
    Fallible(Type),
}

fn protocol_return_kind(output: &ReturnType) -> ProtocolReturnKind {
    let ReturnType::Type(_, ty) = output else {
        return ProtocolReturnKind::Unit;
    };

    let Type::Path(type_path) = &**ty else {
        return ProtocolReturnKind::Value((**ty).clone());
    };

    let Some(segment) = type_path.path.segments.last() else {
        return ProtocolReturnKind::Value((**ty).clone());
    };

    if segment.ident != "Result" {
        return ProtocolReturnKind::Value((**ty).clone());
    }

    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return ProtocolReturnKind::Value((**ty).clone());
    };

    let ok_type = args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty.clone()),
        _ => None,
    });

    ok_type
        .map(ProtocolReturnKind::Fallible)
        .unwrap_or_else(|| ProtocolReturnKind::Value((**ty).clone()))
}

fn auth_return_type(kind: &ProtocolReturnKind) -> proc_macro2::TokenStream {
    match kind {
        ProtocolReturnKind::Unit => quote! { () },
        ProtocolReturnKind::Value(ty) => quote! { ::raster::AuthRef<#ty> },
        ProtocolReturnKind::Fallible(ty) => quote! { ::raster::exec::Result<::raster::AuthRef<#ty>> },
    }
}

fn auth_result_binding(kind: &ProtocolReturnKind, body: &syn::Block) -> proc_macro2::TokenStream {
    match kind {
        ProtocolReturnKind::Unit => quote! {
            let __raster_result: () = (|| #body)();
        },
        ProtocolReturnKind::Value(ty) => quote! {
            let __raster_body_result = (|| #body)();
            let __raster_result: ::raster::AuthRef<#ty> =
                ::raster::into_auth_ref::<#ty, _>(__raster_body_result);
        },
        ProtocolReturnKind::Fallible(ty) => quote! {
            let __raster_body_result = (|| #body)();
            let __raster_result: ::raster::exec::Result<::raster::AuthRef<#ty>> =
                __raster_body_result.map(|value| ::raster::into_auth_ref::<#ty, _>(value));
        },
    }
}

fn trace_output_binding(kind: &ProtocolReturnKind) -> proc_macro2::TokenStream {
    match kind {
        ProtocolReturnKind::Unit => quote! {
            let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&())
                .unwrap_or_default();
        },
        ProtocolReturnKind::Value(_) => quote! {
            let __raster_output_trace = ::raster::auth_ref_trace(&__raster_result)
                .unwrap_or_else(|e| panic!("Failed to trace sequence output: {}", e));
            let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_output_trace)
                .unwrap_or_default();
        },
        ProtocolReturnKind::Fallible(_) => quote! {
            let __raster_output_trace = ::raster::auth_ref_result_trace(&__raster_result)
                .unwrap_or_else(|e| panic!("Failed to trace sequence output: {}", e));
            let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_output_trace)
                .unwrap_or_default();
        },
    }
}

fn materialize_main_result(kind: &ProtocolReturnKind) -> proc_macro2::TokenStream {
    match kind {
        ProtocolReturnKind::Unit => quote! { let __raster_result = __raster_auth_result; },
        ProtocolReturnKind::Value(ty) => quote! {
            let __raster_result: #ty =
                ::raster::materialize_auth_return::<#ty, _>(__raster_auth_result);
        },
        ProtocolReturnKind::Fallible(ty) => quote! {
            let __raster_result: ::raster::exec::Result<#ty> =
                ::raster::materialize_auth_result::<#ty, _>(__raster_auth_result);
        },
    }
}

fn gen_tile_trace_output_serialization() -> proc_macro2::TokenStream {
    quote! {
        let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&result)
            .unwrap_or_else(|e| panic!("Failed to serialize tile output: {}", e));
    }
}

/// Generate only the input deserialization code.
///
/// Returns a TokenStream that deserializes input bytes into the appropriate variables.
fn gen_inputs_deserialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_params(input);
    if params.is_empty() {
        // No arguments - no deserialization needed
        quote! {}
    } else if params.len() == 1 {
        // Single argument - deserialize directly
        let param = &params[0];
        let decode_ty = &param.ty;
        let name = &param.ident;
        quote! {
            let #name: #decode_ty = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let decode_types: Vec<_> = params
            .iter()
            .map(|param| {
                let ty = &param.ty;
                quote! { #ty }
            })
            .collect();
        let names: Vec<_> = params.iter().map(|param| &param.ident).collect();
        let tuple_type = quote! { (#(#decode_types),*) };
        quote! {
            let (#(#names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    }
}

/// Add ABI output serialization to the code pipeline.
///
/// Serializes the user-facing output as raw postcard bytes.
fn gen_output_serialization() -> proc_macro2::TokenStream {
    quote! {
        let output = ::raster::core::postcard::to_allocvec(&result)
            .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to serialize output: {}", e)))?;
    }
}

/// Generate input serialization code for tracing in the original function.
///
/// This serializes the typed input parameters to bytes for the trace emission.
fn gen_input_serialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_params(input);

    let trace_value_defs: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let trace_value_ident = format_ident!("__raster_input_value_{}", name);
            let external_info_ident = external_info_ident(param);
            let internal_info_ident = internal_info_ident(param);
            quote! {
                let #trace_value_ident = if #external_info_ident.is_some() {
                    ::raster::core::trace::FnInputValue::ExternalBinding
                } else if #internal_info_ident.is_some() {
                    ::raster::core::trace::FnInputValue::InternalBinding
                } else {
                    ::raster::core::trace::FnInputValue::Inline(
                        ::raster::core::postcard::to_allocvec(&#name).unwrap_or_default()
                    )
                };
            }
        })
        .collect();

    let input_arg_defs: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let name_str = name.to_string();
            let ty = &param.ty;
            let ty_str = ty.to_token_stream().to_string();
            quote! {
                ::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(#name_str),
                    ty: ::raster::alloc::string::String::from(#ty_str),
                }
            }
        })
        .collect();

    let trace_values: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let trace_value_ident = format_ident!("__raster_input_value_{}", name);
            quote! { #trace_value_ident }
        })
        .collect();

    let external_binding_entries: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let external_info_ident = external_info_ident(param);
            quote! {
                if let ::core::option::Option::Some(__raster_external_info) = #external_info_ident.clone() {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::ExternalData {
                            name: __raster_external_info.name,
                            commitment: __raster_external_info
                                .commitment
                                .map(|value| value.into_bytes())
                                .unwrap_or_default(),
                            tree_root: __raster_external_info.selected.proof.root_hash.clone(),
                            selector: __raster_external_info.selector,
                            selected: __raster_external_info.selected,
                        }
                    );
                }
            }
        })
        .collect();

    let internal_binding_entries: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let internal_info_ident = internal_info_ident(param);
            quote! {
                if let ::core::option::Option::Some(__raster_internal_info) = #internal_info_ident.clone() {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::InternalData {
                            coordinates: __raster_internal_info.reference.coordinates,
                            commitment: __raster_internal_info.reference.commitment,
                        }
                    );
                }
            }
        })
        .collect();

    let input_bytes = if params.is_empty() {
        quote! {
            let __raster_input_bytes: ::raster::alloc::vec::Vec<u8> = ::raster::alloc::vec::Vec::new();
        }
    } else if params.len() == 1 {
        let param = &params[0];
        let name = &param.ident;
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&#name)
                .unwrap_or_default();
        }
    } else {
        let payloads: Vec<_> = params
            .iter()
            .map(|param| {
                let name = &param.ident;
                quote! { &#name }
            })
            .collect();
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&(#(#payloads),*))
                .unwrap_or_default();
        }
    };

    quote! {
        #(#trace_value_defs)*

        let __raster_input_args: ::raster::alloc::vec::Vec<::raster::core::trace::FnInputArg> = ::raster::alloc::vec![
            #(#input_arg_defs),*
        ];

        #input_bytes

        let mut __raster_external = ::raster::alloc::collections::BTreeMap::new();
        #(#external_binding_entries)*
        let mut __raster_internal = ::raster::alloc::collections::BTreeMap::new();
        #(#internal_binding_entries)*

        let __raster_input = ::core::option::Option::Some(
            ::raster::core::trace::FnInput {
                data: __raster_input_bytes,
                values: ::raster::alloc::vec![#(#trace_values),*],
                args: __raster_input_args,
                external: __raster_external,
                internal: __raster_internal,
            }
        );
    }
}

/// Generate only the function call code.
///
/// Returns a TokenStream that calls the function and stores the result.
fn gen_function_call(target_fn: &syn::Ident, input: &ItemFn) -> proc_macro2::TokenStream {
    let param_names: Vec<syn::Ident> = extract_params(input)
        .into_iter()
        .map(|param| param.ident)
        .collect();

    if param_names.is_empty() {
        quote! { let result = #target_fn(); }
    } else {
        quote! { let result = #target_fn(#(#param_names),*); }
    }
}

fn is_call_macro(expr_macro: &syn::ExprMacro) -> bool {
    expr_macro.mac.path.is_ident("call")
}

fn is_call_seq_macro(expr_macro: &syn::ExprMacro) -> bool {
    expr_macro.mac.path.is_ident("call_seq")
}

struct SequenceCallInput {
    callee: syn::Ident,
    args: syn::punctuated::Punctuated<Expr, Token![,]>,
}

impl Parse for SequenceCallInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let callee: syn::Ident = input.parse()?;
        let mut args = syn::punctuated::Punctuated::new();
        if input.parse::<Option<Token![,]>>()?.is_some() {
            args = syn::punctuated::Punctuated::parse_terminated(input)?;
        }
        Ok(Self { callee, args })
    }
}

fn rewrite_call_seq_macro(expr_macro: &syn::ExprMacro) -> Expr {
    let input = syn::parse2::<SequenceCallInput>(expr_macro.mac.tokens.clone()).unwrap_or_else(|_| {
        panic!("call_seq! expects an identifier callee followed by zero or more arguments")
    });
    let hidden = format_ident!("__raster_sequence_auth_{}", input.callee);
    let args = input.args;
    syn::parse_quote! {
        #hidden(#args)
    }
}

fn rewrite_sequence_stmt(stmt: &mut syn::Stmt) {
    match stmt {
        syn::Stmt::Local(local) => {
            if let Some(init) = &mut local.init {
                rewrite_sequence_expr(&mut init.expr);
                if let Some((_, diverge)) = &mut init.diverge {
                    rewrite_sequence_expr(diverge);
                }
            }
        }
        syn::Stmt::Item(_) => {}
        syn::Stmt::Macro(stmt_macro) => {
            let expr_macro = syn::ExprMacro {
                attrs: Vec::new(),
                mac: stmt_macro.mac.clone(),
            };
            if is_call_macro(&expr_macro) {
                let original = Expr::Macro(expr_macro);
                *stmt = syn::parse_quote! {
                    ::raster::__private::bind_infallible_call(#original);
                };
            } else if is_call_seq_macro(&expr_macro) {
                let rewritten = rewrite_call_seq_macro(&expr_macro);
                *stmt = syn::parse_quote! {
                    #rewritten;
                };
            }
        }
        syn::Stmt::Expr(expr, _) => rewrite_sequence_expr(expr),
    }
}

fn rewrite_sequence_block(block: &mut syn::Block) {
    for stmt in block.stmts.iter_mut() {
        rewrite_sequence_stmt(stmt);
    }
}

fn rewrite_sequence_expr(expr: &mut Expr) {
    match expr {
        Expr::Array(expr_array) => {
            for element in expr_array.elems.iter_mut() {
                rewrite_sequence_expr(element);
            }
        }
        Expr::Assign(expr_assign) => {
            rewrite_sequence_expr(&mut expr_assign.left);
            rewrite_sequence_expr(&mut expr_assign.right);
        }
        Expr::Async(expr_async) => rewrite_sequence_block(&mut expr_async.block),
        Expr::Await(expr_await) => rewrite_sequence_expr(&mut expr_await.base),
        Expr::Binary(expr_binary) => {
            rewrite_sequence_expr(&mut expr_binary.left);
            rewrite_sequence_expr(&mut expr_binary.right);
        }
        Expr::Block(expr_block) => rewrite_sequence_block(&mut expr_block.block),
        Expr::Break(expr_break) => {
            if let Some(value) = &mut expr_break.expr {
                rewrite_sequence_expr(value);
            }
        }
        Expr::Call(expr_call) => {
            rewrite_sequence_expr(&mut expr_call.func);
            for argument in expr_call.args.iter_mut() {
                rewrite_sequence_expr(argument);
            }
        }
        Expr::Cast(expr_cast) => rewrite_sequence_expr(&mut expr_cast.expr),
        Expr::Closure(expr_closure) => rewrite_sequence_expr(&mut expr_closure.body),
        Expr::Field(expr_field) => rewrite_sequence_expr(&mut expr_field.base),
        Expr::ForLoop(expr_for_loop) => {
            rewrite_sequence_expr(&mut expr_for_loop.expr);
            rewrite_sequence_block(&mut expr_for_loop.body);
        }
        Expr::Group(expr_group) => rewrite_sequence_expr(&mut expr_group.expr),
        Expr::If(expr_if) => {
            rewrite_sequence_expr(&mut expr_if.cond);
            rewrite_sequence_block(&mut expr_if.then_branch);
            if let Some((_, else_branch)) = &mut expr_if.else_branch {
                rewrite_sequence_expr(else_branch);
            }
        }
        Expr::Index(expr_index) => {
            rewrite_sequence_expr(&mut expr_index.expr);
            rewrite_sequence_expr(&mut expr_index.index);
        }
        Expr::Infer(_) => {}
        Expr::Let(expr_let) => rewrite_sequence_expr(&mut expr_let.expr),
        Expr::Lit(_) => {}
        Expr::Loop(expr_loop) => rewrite_sequence_block(&mut expr_loop.body),
        Expr::Macro(expr_macro) => {
            if is_call_macro(expr_macro) {
                let original = expr.clone();
                *expr = syn::parse_quote! {
                    ::raster::__private::bind_infallible_call(#original)
                };
            } else if is_call_seq_macro(expr_macro) {
                *expr = rewrite_call_seq_macro(expr_macro);
            }
        }
        Expr::Match(expr_match) => {
            rewrite_sequence_expr(&mut expr_match.expr);
            for arm in expr_match.arms.iter_mut() {
                if let Some((_, guard)) = &mut arm.guard {
                    rewrite_sequence_expr(guard);
                }
                rewrite_sequence_expr(&mut arm.body);
            }
        }
        Expr::MethodCall(expr_method_call) => {
            rewrite_sequence_expr(&mut expr_method_call.receiver);
            for argument in expr_method_call.args.iter_mut() {
                rewrite_sequence_expr(argument);
            }
        }
        Expr::Paren(expr_paren) => rewrite_sequence_expr(&mut expr_paren.expr),
        Expr::Path(_) => {}
        Expr::Range(expr_range) => {
            if let Some(from) = &mut expr_range.start {
                rewrite_sequence_expr(from);
            }
            if let Some(to) = &mut expr_range.end {
                rewrite_sequence_expr(to);
            }
        }
        Expr::Reference(expr_reference) => rewrite_sequence_expr(&mut expr_reference.expr),
        Expr::Repeat(expr_repeat) => {
            rewrite_sequence_expr(&mut expr_repeat.expr);
            rewrite_sequence_expr(&mut expr_repeat.len);
        }
        Expr::Return(expr_return) => {
            if let Some(value) = &mut expr_return.expr {
                rewrite_sequence_expr(value);
            }
        }
        Expr::Struct(expr_struct) => {
            for field in expr_struct.fields.iter_mut() {
                rewrite_sequence_expr(&mut field.expr);
            }
            if let Some(rest) = &mut expr_struct.rest {
                rewrite_sequence_expr(rest);
            }
        }
        Expr::Try(expr_try) => {
            if let Expr::Macro(expr_macro) = expr_try.expr.as_ref() {
                if is_call_macro(expr_macro) {
                    let original = expr_try.expr.as_ref().clone();
                    expr_try.expr = Box::new(syn::parse_quote! {
                        ::raster::__private::bind_fallible_call(#original)
                    });
                    return;
                } else if is_call_seq_macro(expr_macro) {
                    expr_try.expr = Box::new(rewrite_call_seq_macro(expr_macro));
                    return;
                }
            }
            rewrite_sequence_expr(&mut expr_try.expr);
        }
        Expr::TryBlock(expr_try_block) => rewrite_sequence_block(&mut expr_try_block.block),
        Expr::Tuple(expr_tuple) => {
            for element in expr_tuple.elems.iter_mut() {
                rewrite_sequence_expr(element);
            }
        }
        Expr::Unary(expr_unary) => rewrite_sequence_expr(&mut expr_unary.expr),
        Expr::Unsafe(expr_unsafe) => rewrite_sequence_block(&mut expr_unsafe.block),
        Expr::While(expr_while) => {
            rewrite_sequence_expr(&mut expr_while.cond);
            rewrite_sequence_block(&mut expr_while.body);
        }
        Expr::Yield(expr_yield) => {
            if let Some(value) = &mut expr_yield.expr {
                rewrite_sequence_expr(value);
            }
        }
        _ => {}
    }
}

/// Parses tile attributes from the macro invocation.
///
/// Uses named argument `kind` for tile type: `#[tile(kind = iter)]` or `#[tile(kind = recur)]`.
struct TileAttrs {
    /// Tile type: "iter" (default) or "recur".
    tile_type: String,
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
    description: Option<String>,
}

impl TileAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = TileAttrs {
            tile_type: "iter".to_string(), // default to iter
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        if attr.is_empty() {
            return attrs; // Default to "iter" if no arguments
        }

        // Parse comma-separated key=value pairs
        let attr_str = attr.to_string();

        for part in attr_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                match key {
                    "kind" => match value {
                        "iter" | "recur" => attrs.tile_type = value.to_string(),
                        _ => panic!("Unknown tile kind '{}'. Valid kinds: iter, recur", value),
                    },
                    "estimated_cycles" => {
                        attrs.estimated_cycles = value.parse().ok();
                    }
                    "max_memory" => {
                        attrs.max_memory = value.parse().ok();
                    }
                    "description" => {
                        attrs.description = Some(value.to_string());
                    }
                    _ => {}
                }
            }
        }

        attrs
    }
}

/// Marks a function as a Raster tile.
///
/// This macro:
/// 1. Injects tracing code into the original function (for std targets)
/// 2. Generates an ABI wrapper that handles postcard serialization/deserialization
///
/// # Attributes
/// - `kind = iter` - Standard iterative tile (default if not specified)
/// - `kind = recur` - Recursive tile for stateful computations
/// - `estimated_cycles = N` - Expected cycle count for resource estimation
/// - `max_memory = N` - Maximum memory usage in bytes
/// - `description = "..."` - Human-readable description
///
/// # Example
/// ```ignore
/// #[tile]
/// fn compute(input: u64) -> u64 {
///     input * 2
/// }
///
/// #[tile(kind = iter, estimated_cycles = 1000, description = "Greets a user")]
/// fn greet(name: String) -> String {
///     format!("Hello, {}!", name)
/// }
///
/// #[tile(kind = recur)]
/// fn iterate(state: State) -> State {
///     // recursive computation
/// }
/// ```
#[proc_macro_attribute]
pub fn tile(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name_str = input_fn.sig.ident.to_string();
    validate_protocol_return_type(&input_fn);

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_attrs = &input_fn.attrs;
    let fn_body = &input_fn.block;

    let function_wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name_str);
    let implementation_name = format_ident!("__raster_tile_impl_{}", fn_name_str);

    let attrs = TileAttrs::parse(attr);

    // Generate optional metadata fields
    let _estimated_cycles_expr = match attrs.estimated_cycles {
        Some(cycles) => quote! { ::core::option::Option::Some(#cycles) },
        None => quote! { ::core::option::Option::None },
    };

    let _max_memory_expr = match attrs.max_memory {
        Some(memory) => quote! { ::core::option::Option::Some(#memory) },
        None => quote! { ::core::option::Option::None },
    };

    // Generate deserialization and function call
    let inputs_deserialization = gen_inputs_deserialization(&input_fn);
    let function_call = gen_function_call(&implementation_name, &input_fn);
    let output_serialization = gen_output_serialization();
    let trace_output_serialization = gen_tile_trace_output_serialization();
    let auth_value_materialization = gen_auth_value_materialization(&input_fn);

    let mut exposed_sig = input_fn.sig.clone();
    rewrite_into_auth_value_args(&mut exposed_sig);

    let mut implementation_sig = input_fn.sig.clone();
    implementation_sig.ident = implementation_name.clone();

    // For recursive tiles, also generate a macro with the same name that allows `tile_name!(args)` syntax
    let _recursive_macro = if attrs.tile_type == "recur" {
        let macro_name = format_ident!("{}", fn_name);
        quote! {
            /// Macro wrapper for recursive tile invocation.
            /// Use `tile_name!(args)` to invoke this recursive tile.
            /// For native execution, this simply calls the underlying function.
            /// The `!` syntax signals to the CFS compiler that this tile should
            /// be executed recursively until its first output returns true.
            #[macro_export]
            macro_rules! #macro_name {
                ($($args:expr),* $(,)?) => {
                    #fn_name($($args),*)
                };
            }
        }
    } else {
        quote! {}
    };

    // Generate output type expression
    let output_type_expr = match &input_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    // Generate input serialization code for tracing
    let input_serialization = gen_input_serialization(&input_fn);

    let implementation_function = quote! {
        #implementation_sig #fn_body
    };

    let original_function = quote! {
        #(#fn_attrs)*
        #fn_vis #exposed_sig {
            #auth_value_materialization

            // On std + non-riscv32: wrap body in closure for tracing
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                // Serialize inputs for tracing
                #input_serialization

                #function_call

                // Serialize output and emit TraceEvent::TileExec
                #trace_output_serialization

                let __raster_output = ::core::option::Option::Some(
                    ::raster::core::trace::FnOutput {
                        data: __raster_output_bytes,
                        ty: ::raster::alloc::string::String::from(#output_type_expr),
                    }
                );

                let __raster_record = ::raster::core::trace::FnCallRecord {
                    fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                    input: __raster_input,
                    output: __raster_output,
                };
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::TileExec(
                    __raster_record,
                ));

                return result;
            }

            // On riscv32 or no-std: just execute original body directly
            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            {
                #function_call
                result
            }
        }
    };

    TokenStream::from(quote! {
        #implementation_function

        // Original function with tracing injected
        #original_function

        // Generate the ABI wrapper function (available on all platforms, no_std compatible).
        // Its Result channel is reserved for Raster runtime/protocol failures.
        pub fn #function_wrapper_name(input: &[u8]) -> ::raster::core::Result<::raster::alloc::vec::Vec<u8>> {
            #inputs_deserialization

            #function_call

            #output_serialization

            Ok(output)
        }
    })
}

/// Parses optional sequence attributes from the macro invocation.
struct SequenceAttrs {
    description: Option<String>,
}

impl SequenceAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = SequenceAttrs { description: None };

        if attr.is_empty() {
            return attrs;
        }

        // Parse comma-separated key=value pairs
        let attr_str = attr.to_string();
        for part in attr_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                if key == "description" {
                    attrs.description = Some(value.to_string());
                }
            }
        }

        attrs
    }
}

/// Generates the sequence-wrapped body: either tracing (SequenceStart/body/SequenceEnd) or plain body depending on cfg.
fn gen_sequence_wrapped_body(
    fn_name_str: &str,
    item_fn: &ItemFn,
    return_kind: &ProtocolReturnKind,
) -> proc_macro2::TokenStream {
    let body = &item_fn.block;
    let input_serialization = gen_sequence_input_serialization(&item_fn);
    let auth_result_binding = auth_result_binding(return_kind, body);
    let trace_output_binding = trace_output_binding(return_kind);

    let output_type_expr = match &item_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    quote! {
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        {
            let __raster_sequence_scope_guard =
                ::raster::__private::SequenceScopeGuard::enter(#fn_name_str);
            #input_serialization

            let mut __raster_record = ::raster::core::trace::FnCallRecord {
                fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                input: __raster_input,
                output: ::core::option::Option::None,
            };
            ::raster::publish_trace_event(::raster::core::trace::TraceEvent::SequenceStart(
                __raster_record.clone(),
            ));
            #auth_result_binding
            #trace_output_binding
            __raster_record.output = ::core::option::Option::Some(::raster::core::trace::FnOutput::new(
                __raster_output_bytes,
                ::raster::alloc::string::String::from(#output_type_expr),
            ));
            ::raster::publish_trace_event(::raster::core::trace::TraceEvent::SequenceEnd(
                __raster_record,
            ));
            __raster_result
        }

        #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
        {
            let __raster_sequence_scope_guard =
                ::raster::__private::SequenceScopeGuard::enter(#fn_name_str);
            #auth_result_binding
            __raster_result
        }
    }
}

/// Declares a sequence of tiles with linear control flow.
///
/// The `#[sequence]` macro parses the function body to extract tile calls
/// in the order they appear. The function remains callable for native execution,
/// and the sequence is registered for use with `cargo raster preview`.
///
/// When the function is named **`main`**, it is the program entry point: the macro expands to
/// `fn main() { init(); sequence_wrapped_body; finish(); }`.
///
/// # Attributes
/// - `description = "..."` - Human-readable description of the sequence
///
/// # Example (entry point)
/// ```ignore
/// #[raster::sequence]
/// fn main() {
///     let name = raster::select!(String, raster::external!(String, "name"));
///     let result = greet_sequence(name);
///     println!("{}", result);
/// }
/// ```
///
/// # Example (nested sequence)
/// ```ignore
/// #[sequence]
/// fn greet_sequence(name: String) -> String {
///     exclaim(greet(name))
/// }
/// ```
#[proc_macro_attribute]
pub fn sequence(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut item_fn = parse_macro_input!(item as ItemFn);
    rewrite_sequence_block(&mut item_fn.block);
    let fn_name_str = item_fn.sig.ident.to_string();
    validate_protocol_return_type(&item_fn);
    let params = extract_params(&item_fn);

    let fn_vis = &item_fn.vis;
    let fn_attrs = &item_fn.attrs;
    let _attrs = SequenceAttrs::parse(attr);
    let return_kind = protocol_return_kind(&item_fn.sig.output);

    let expanded = if item_fn.sig.ident == "main" {
        if !params.is_empty() {
            panic!(
                "`#[sequence] fn main` must not declare parameters. Bind committed inputs explicitly inside the body with external!(...)."
            );
        }

        let output = &item_fn.sig.output;
        let auth_name = format_ident!("__raster_sequence_auth_main");
        let auth_output = auth_return_type(&return_kind);
        let body = gen_sequence_wrapped_body("main", &item_fn, &return_kind);
        let materialize_result = materialize_main_result(&return_kind);
        quote! {
            fn #auth_name() -> #auth_output {
                #body
            }

            #(#fn_attrs)*
            fn main() #output {
                ::raster::init();

                let __raster_auth_result = #auth_name();
                #materialize_result

                ::raster::finish();
                __raster_result
            }
        }
    } else {
        let auth_name = format_ident!("__raster_sequence_auth_{}", fn_name_str);
        let auth_output = auth_return_type(&return_kind);
        let body = gen_sequence_wrapped_body(&fn_name_str, &item_fn, &return_kind);
        let arg_idents: Vec<_> = params
            .iter()
            .enumerate()
            .map(|(index, _)| sequence_arg_ident(index))
            .collect();
        let wrapper_params: Vec<_> = params
            .iter()
            .zip(arg_idents.iter())
            .map(|(param, arg_ident)| {
                let name = &param.ident;
                quote! { #name: #arg_ident }
            })
            .collect();
        let wrapper_where: Vec<_> = params
            .iter()
            .zip(arg_idents.iter())
            .map(|(param, arg_ident)| {
                let ty = &param.ty;
                quote! { #arg_ident: ::raster::IntoAuthRef<#ty> }
            })
            .collect();
        let conversions: Vec<_> = params
            .iter()
            .map(|param| {
                let name = &param.ident;
                let ty = &param.ty;
                quote! {
                    let #name = ::raster::into_auth_ref::<#ty, _>(#name);
                }
            })
            .collect();

        if params.is_empty() {
            quote! {
                #(#fn_attrs)*
                #fn_vis fn #auth_name() -> #auth_output {
                    #body
                }
            }
        } else {
            quote! {
                #(#fn_attrs)*
                #fn_vis fn #auth_name<#(#arg_idents),*>(#(#wrapper_params),*) -> #auth_output
                where
                    #(#wrapper_where),*
                {
                    #(#conversions)*
                    #body
                }
            }
        }
    };

    TokenStream::from(expanded)
}

fn split_selector_expr(expr: Expr) -> (Expr, Vec<proc_macro2::TokenStream>) {
    match expr {
        Expr::Field(ExprField { base, member, .. }) => {
            let (base_expr, mut segments) = split_selector_expr(*base);
            let segment = match member {
                syn::Member::Named(ident) => {
                    let name = ident.to_string();
                    quote! { ::raster::SelectorSegment::Field(::raster::alloc::string::String::from(#name)) }
                }
                syn::Member::Unnamed(index) => {
                    let value = index.index;
                    quote! { ::raster::SelectorSegment::Index(#value as u64) }
                }
            };
            segments.push(segment);
            (base_expr, segments)
        }
        Expr::Index(ExprIndex { expr, index, .. }) => {
            let (base_expr, mut segments) = split_selector_expr(*expr);
            let Expr::Lit(expr_lit) = *index else {
                panic!("select! only supports integer literal indexes");
            };
            let syn::Lit::Int(LitInt { .. }) = &expr_lit.lit else {
                panic!("select! only supports integer literal indexes");
            };
            let value = expr_lit.lit.to_token_stream();
            segments.push(quote! { ::raster::SelectorSegment::Index((#value) as u64) });
            (base_expr, segments)
        }
        other => (other, Vec::new()),
    }
}

struct SelectInput {
    selected_ty: Type,
    expr: Expr,
}

impl Parse for SelectInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            selected_ty: input.parse()?,
            expr: {
                input.parse::<Token![,]>()?;
                input.parse()?
            },
        })
    }
}

#[proc_macro]
pub fn select(item: TokenStream) -> TokenStream {
    let SelectInput { selected_ty, expr } = parse_macro_input!(item as SelectInput);
    let (base_expr, segments) = split_selector_expr(expr);

    TokenStream::from(quote! {
        ::raster::select_source(
            #base_expr,
            ::raster::typed_selector_path::<_, #selected_ty>(
                ::raster::SelectorPath::new(::raster::alloc::vec![#(#segments),*]),
            ),
        )
    })
}

#[proc_macro_derive(Selectable, attributes(schema))]
pub fn derive_selectable(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::DeriveInput);
    let ident = &input.ident;
    let generics = input.generics.clone();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(fields) => fields.named.iter().collect::<Vec<_>>(),
            _ => panic!("Selectable can only be derived for structs with named fields"),
        },
        _ => panic!("Selectable can only be derived for structs"),
    };

    let schema_fields: Vec<_> = fields
        .iter()
        .map(|field| {
            let field_ident = field.ident.as_ref().expect("named field");
            let field_ty = &field.ty;
            let field_name = field_ident.to_string();
            let label = parse_schema_tag(&field.attrs)
                .map(|tag| tag.to_string())
                .unwrap_or_else(|| field_name.clone());
            quote! {
                ::raster::core::input::SchemaField::new(
                    #field_name,
                    #label,
                    <#field_ty as ::raster::core::input::Selectable>::schema(),
                )
            }
        })
        .collect();

    TokenStream::from(quote! {
        impl #impl_generics ::raster::core::input::Selectable for #ident #ty_generics #where_clause {
            fn schema() -> ::raster::core::input::SchemaNode {
                ::raster::core::input::SchemaNode::Struct {
                    type_name: ::raster::alloc::string::String::from(stringify!(#ident)),
                    fields: ::raster::alloc::vec![#(#schema_fields),*],
                }
            }
        }
    })
}
