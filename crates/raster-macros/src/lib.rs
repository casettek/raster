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

fn draft_inner_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Draft" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    })
}

fn is_draft_type(ty: &Type) -> bool {
    draft_inner_type(ty).is_some()
}

fn recur_input_inner_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "RecurInput" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    })
}

fn recur_state_inner_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "RecurState" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    })
}

fn recur_output_inner_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "RecurOutput" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    })
}

fn recur_control_inner_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "RecurControl" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    })
}

fn recur_control_draft_inner_type(ty: &Type) -> Option<Type> {
    recur_control_inner_type(ty).and_then(|inner| draft_inner_type(&inner))
}

fn recur_control_recur_output_inner_type(ty: &Type) -> Option<Type> {
    recur_control_inner_type(ty).and_then(|inner| recur_output_inner_type(&inner))
}

fn recur_control_recur_state_inner_type(ty: &Type) -> Option<Type> {
    recur_control_inner_type(ty).and_then(|inner| recur_state_inner_type(&inner))
}

fn recur_state_output_parts(ty: &Type) -> Option<(Type, Type)> {
    let Type::Tuple(tuple) = ty else {
        return None;
    };
    if tuple.elems.len() != 2 {
        return None;
    }
    let mut elems = tuple.elems.iter();
    let state_ty = elems.next()?.clone();
    let output_ty = elems.next()?.clone();
    let recur_state_ty = recur_state_inner_type(&state_ty)?;
    recur_output_inner_type(&output_ty)?;
    Some((recur_state_ty, output_ty))
}

fn recur_control_state_output_parts(ty: &Type) -> Option<(Type, Type)> {
    recur_control_inner_type(ty).and_then(|inner| recur_state_output_parts(&inner))
}

fn is_u64_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(type_path) if {
        type_path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "u64")
    })
}

fn types_equivalent(left: &Type, right: &Type) -> bool {
    left.to_token_stream().to_string() == right.to_token_stream().to_string()
}

#[derive(Clone)]
enum ParamProtocolKind {
    AuthValue(Type),
    Draft(Type),
    RecurOutput(Type),
}

fn param_protocol_kind(ty: &Type) -> ParamProtocolKind {
    if is_draft_type(ty) {
        ParamProtocolKind::Draft(ty.clone())
    } else if recur_output_inner_type(ty).is_some() {
        ParamProtocolKind::RecurOutput(ty.clone())
    } else {
        ParamProtocolKind::AuthValue(ty.clone())
    }
}

fn rewrite_into_auth_value_args(sig: &mut syn::Signature) {
    for arg in sig.inputs.iter_mut() {
        if let FnArg::Typed(pat_type) = arg {
            let ty = &pat_type.ty;
            if !is_draft_type(ty) && recur_output_inner_type(ty).is_none() {
                pat_type.ty = syn::parse_quote!(impl ::raster::IntoAuthValue<#ty>);
            }
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

fn tile_call_binding_marker_ident(fn_name: &syn::Ident) -> syn::Ident {
    format_ident!("__RasterTileCallBinding_{}", fn_name)
}

fn gen_auth_value_materialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_params(input);
    let resolutions: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let protocol_kind = param_protocol_kind(&param.ty);
            let external_info_ident = external_info_ident(param);
            let internal_info_ident = internal_info_ident(param);
            match protocol_kind {
                ParamProtocolKind::AuthValue(value_ty) => quote! {
                    let __raster_auth_value = ::raster::into_auth_value::<#value_ty, _>(#name)
                        .unwrap_or_else(|e| panic!("Failed to materialize auth value for argument '{}': {}", stringify!(#name), e));
                    let #external_info_ident = __raster_auth_value.as_external().cloned();
                    let #internal_info_ident = __raster_auth_value.as_internal().cloned();
                    let #name: #value_ty = __raster_auth_value.into_inner();
                },
                ParamProtocolKind::Draft(value_ty) | ParamProtocolKind::RecurOutput(value_ty) => quote! {
                    let #external_info_ident: ::core::option::Option<::raster::ExternalValue<#value_ty>> = ::core::option::Option::None;
                    let #internal_info_ident: ::core::option::Option<::raster::InternalValue<#value_ty>> = ::core::option::Option::None;
                },
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
                if let ::core::option::Option::Some(__raster_external_info) = &#external_info_ident {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        __raster_external_info.clone(),
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
                if let ::core::option::Option::Some(__raster_internal_info) = &#internal_info_ident {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        __raster_internal_info.clone(),
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

fn vec_element_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Vec" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    })
}

fn fallible_result_message() -> &'static str {
    "Fallible Raster functions must return bare `Result<T>` / `Result<T, String>`, `raster::exec::Result<T>`, or `std::result::Result<T, String>` / `core::result::Result<T, String>`."
}

fn draft_result_message() -> &'static str {
    "Draft-carrying Raster functions must return `Draft<S>` directly; `Result<Draft<S>, String>` is not supported."
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
        if type_args.first().is_some_and(|ok_type| {
            is_draft_type(ok_type) || recur_control_draft_inner_type(ok_type).is_some()
        }) {
            panic!("{}", draft_result_message());
        }
        return;
    }

    if is_raster_exec_result_path(&type_path.path) {
        match type_args.as_slice() {
            [_ok_type] => {}
            _ => panic!("{}", fallible_result_message()),
        }
        if type_args.first().is_some_and(|ok_type| {
            is_draft_type(ok_type) || recur_control_draft_inner_type(ok_type).is_some()
        }) {
            panic!("{}", draft_result_message());
        }
        return;
    }

    if is_std_result_path(&type_path.path) {
        match type_args.as_slice() {
            [_ok_type, err_type] if is_string_type(err_type) => {}
            _ => panic!("{}", fallible_result_message()),
        }
        if type_args.first().is_some_and(|ok_type| {
            is_draft_type(ok_type) || recur_control_draft_inner_type(ok_type).is_some()
        }) {
            panic!("{}", draft_result_message());
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
    Draft(Type),
    RecurControlDraft(Type),
    RecurOutput(Type),
    RecurControlRecurOutput(Type),
    RecurState(Type),
    RecurControlRecurState(Type),
    RecurStateOutput(Type),
    RecurControlRecurStateOutput(Type),
}

fn protocol_return_kind(output: &ReturnType) -> ProtocolReturnKind {
    let ReturnType::Type(_, ty) = output else {
        return ProtocolReturnKind::Unit;
    };

    if is_draft_type(ty) {
        return ProtocolReturnKind::Draft((**ty).clone());
    }

    if recur_control_draft_inner_type(ty).is_some() {
        return ProtocolReturnKind::RecurControlDraft((**ty).clone());
    }

    if recur_output_inner_type(ty).is_some() {
        return ProtocolReturnKind::RecurOutput((**ty).clone());
    }

    if recur_control_recur_output_inner_type(ty).is_some() {
        return ProtocolReturnKind::RecurControlRecurOutput((**ty).clone());
    }

    if recur_state_inner_type(ty).is_some() {
        return ProtocolReturnKind::RecurState((**ty).clone());
    }

    if recur_control_recur_state_inner_type(ty).is_some() {
        return ProtocolReturnKind::RecurControlRecurState((**ty).clone());
    }

    if recur_state_output_parts(ty).is_some() {
        return ProtocolReturnKind::RecurStateOutput((**ty).clone());
    }

    if recur_control_state_output_parts(ty).is_some() {
        return ProtocolReturnKind::RecurControlRecurStateOutput((**ty).clone());
    }

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
        ProtocolReturnKind::Fallible(ty) => {
            quote! { ::raster::exec::Result<::raster::AuthRef<#ty>> }
        }
        ProtocolReturnKind::Draft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurControlDraft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurControlRecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
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
        ProtocolReturnKind::Draft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurControlDraft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurControlRecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
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
        ProtocolReturnKind::Draft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurControlDraft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurControlRecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
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
        ProtocolReturnKind::Draft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurControlDraft(_) => {
            panic!("`#[sequence]` functions must finalize Draft handles before returning")
        }
        ProtocolReturnKind::RecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurControlRecurState(_) => {
            panic!("`#[sequence]` functions must finalize recur states before returning")
        }
        ProtocolReturnKind::RecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
        ProtocolReturnKind::RecurControlRecurStateOutput(_) => {
            panic!("`#[sequence]` functions must finalize recur outputs before returning")
        }
    }
}

fn gen_tile_call_binding_marker(
    marker_ident: &syn::Ident,
    return_kind: &ProtocolReturnKind,
    output: &ReturnType,
) -> proc_macro2::TokenStream {
    let return_ty = match output {
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => quote! { #ty },
    };

    match return_kind {
        ProtocolReturnKind::Unit | ProtocolReturnKind::Value(_) => quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            pub struct #marker_ident;

            impl ::raster::__private::TileCallBinding<#return_ty> for #marker_ident {
                type Output = ::raster::AuthRef<#return_ty>;

                fn bind(result: #return_ty) -> Self::Output {
                    ::raster::__private::bind_infallible_call(result)
                }
            }
        },
        ProtocolReturnKind::Fallible(ok_ty) => quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            pub struct #marker_ident;

            impl ::raster::__private::TileCallBinding<#return_ty> for #marker_ident {
                type Output = ::raster::AuthRef<#return_ty>;

                fn bind(result: #return_ty) -> Self::Output {
                    ::raster::__private::bind_infallible_call(result)
                }
            }

            impl ::raster::__private::TryTileCallBinding<#return_ty> for #marker_ident {
                type Output = ::raster::exec::Result<::raster::AuthRef<#ok_ty>>;

                fn bind(result: #return_ty) -> Self::Output {
                    ::raster::__private::bind_fallible_call(result)
                }
            }
        },
        ProtocolReturnKind::Draft(_)
        | ProtocolReturnKind::RecurControlDraft(_)
        | ProtocolReturnKind::RecurOutput(_)
        | ProtocolReturnKind::RecurControlRecurOutput(_)
        | ProtocolReturnKind::RecurState(_)
        | ProtocolReturnKind::RecurControlRecurState(_)
        | ProtocolReturnKind::RecurStateOutput(_)
        | ProtocolReturnKind::RecurControlRecurStateOutput(_) => quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            pub struct #marker_ident;

            impl ::raster::__private::TileCallBinding<#return_ty> for #marker_ident {
                type Output = #return_ty;

                fn bind(result: #return_ty) -> Self::Output {
                    result
                }
            }
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecurTileMode {
    OutputOnly,
    StateOnly,
    StateOutput,
}

#[derive(Clone)]
struct RecurTileShape {
    mode: RecurTileMode,
    input_param: ParamInfo,
    state_param: Option<ParamInfo>,
    state_inner: Option<Type>,
    extra_params: Vec<ParamInfo>,
    output_schema: Option<Type>,
}

fn validate_recur_tile_shape(input: &ItemFn, return_kind: &ProtocolReturnKind) -> RecurTileShape {
    let params = extract_params(input);
    if params.len() < 2 {
        panic!("`#[tile(kind = recur)]` tiles must accept at least `(input, state)` or `(input, output)`");
    }

    let input_param = params.first().cloned().expect("recur tile input param");
    recur_input_inner_type(&input_param.ty).unwrap_or_else(|| {
        panic!("`#[tile(kind = recur)]` tiles must start with `input: RecurInput<T>`")
    });

    let second_param = params.get(1).cloned().expect("recur tile second param");
    let (mode, state_param, state_inner, output_param, output_schema, extra_start) = if let Some(
        output_schema,
    ) =
        recur_output_inner_type(&second_param.ty)
    {
        if params
            .get(2)
            .is_some_and(|param| recur_state_inner_type(&param.ty).is_some())
        {
            panic!("`#[tile(kind = recur)]` tiles must place `state: RecurState<S>` before `output: RecurOutput<T>`");
        }
        (
            RecurTileMode::OutputOnly,
            None,
            None,
            Some(second_param),
            Some(output_schema),
            2usize,
        )
    } else if let Some(state_inner) = recur_state_inner_type(&second_param.ty) {
        if let Some(third_param) = params.get(2).cloned() {
            if let Some(output_schema) = recur_output_inner_type(&third_param.ty) {
                (
                    RecurTileMode::StateOutput,
                    Some(second_param),
                    Some(state_inner),
                    Some(third_param),
                    Some(output_schema),
                    3usize,
                )
            } else {
                (
                    RecurTileMode::StateOnly,
                    Some(second_param),
                    Some(state_inner),
                    None,
                    None,
                    2usize,
                )
            }
        } else {
            (
                RecurTileMode::StateOnly,
                Some(second_param),
                Some(state_inner),
                None,
                None,
                2usize,
            )
        }
    } else {
        panic!("`#[tile(kind = recur)]` tiles must place `state: RecurState<S>` or `output: RecurOutput<T>` after `input`");
    };

    let extra_params = params[extra_start..].to_vec();

    if extra_params.iter().any(|param| {
        recur_state_inner_type(&param.ty).is_some()
            || recur_output_inner_type(&param.ty).is_some()
            || recur_input_inner_type(&param.ty).is_some()
    }) {
        panic!("`#[tile(kind = recur)]` only supports plain `args...` after recur state/output parameters");
    }

    match mode {
        RecurTileMode::OutputOnly => {
            let output_param = output_param.as_ref().expect("output-only output param");
            match return_kind {
                ProtocolReturnKind::RecurOutput(return_ty) => {
                    if !types_equivalent(return_ty, &output_param.ty) {
                        panic!(
                            "`#[tile(kind = recur)]` output-only tiles must return the same `RecurOutput<S>` type as their output parameter"
                        );
                    }
                }
                ProtocolReturnKind::RecurControlRecurOutput(return_ty) => {
                    let inner_output_ty = recur_control_inner_type(return_ty)
                        .expect("validated recur control output inner type");
                    if !types_equivalent(&inner_output_ty, &output_param.ty) {
                        panic!(
                            "`#[tile(kind = recur)]` output-only tiles must return the same `RecurOutput<S>` type as their output parameter"
                        );
                    }
                }
                _ => {
                    panic!(
                        "`#[tile(kind = recur)]` output-only tiles must return `RecurOutput<S>` or `RecurControl<RecurOutput<S>>`"
                    )
                }
            }
        }
        RecurTileMode::StateOnly => {
            let state_param = state_param.as_ref().expect("state-only state param");
            match return_kind {
                ProtocolReturnKind::RecurState(return_ty) => {
                    if !types_equivalent(return_ty, &state_param.ty) {
                        panic!(
                            "`#[tile(kind = recur)]` state-only tiles must return the same `RecurState<S>` type as their state parameter"
                        );
                    }
                }
                ProtocolReturnKind::RecurControlRecurState(return_ty) => {
                    let inner_state_ty = recur_control_inner_type(return_ty)
                        .expect("validated recur control state inner type");
                    if !types_equivalent(&inner_state_ty, &state_param.ty) {
                        panic!(
                            "`#[tile(kind = recur)]` state-only tiles must return the same `RecurState<S>` type as their state parameter"
                        );
                    }
                }
                _ => {
                    panic!(
                        "`#[tile(kind = recur)]` state-only tiles must return `RecurState<S>` or `RecurControl<RecurState<S>>`"
                    )
                }
            }
        }
        RecurTileMode::StateOutput => {
            let state_param = state_param.as_ref().expect("state+output state param");
            let output_param = output_param.as_ref().expect("state+output output param");
            match return_kind {
                ProtocolReturnKind::RecurStateOutput(return_ty) => {
                    let (return_state_ty, return_output_ty) = recur_state_output_parts(return_ty)
                        .expect("validated recur state output tuple");
                    let state_inner =
                        recur_state_inner_type(&state_param.ty).expect("recur state inner type");
                    if !types_equivalent(&return_state_ty, &state_inner)
                        || !types_equivalent(&return_output_ty, &output_param.ty)
                    {
                        panic!(
                            "`#[tile(kind = recur)]` state+output tiles must return `(RecurState<S>, RecurOutput<T>)` matching their parameters"
                        );
                    }
                }
                ProtocolReturnKind::RecurControlRecurStateOutput(return_ty) => {
                    let (return_state_ty, return_output_ty) =
                        recur_control_state_output_parts(return_ty)
                            .expect("validated recur control state output tuple");
                    let state_inner =
                        recur_state_inner_type(&state_param.ty).expect("recur state inner type");
                    if !types_equivalent(&return_state_ty, &state_inner)
                        || !types_equivalent(&return_output_ty, &output_param.ty)
                    {
                        panic!(
                            "`#[tile(kind = recur)]` state+output tiles must return `(RecurState<S>, RecurOutput<T>)` matching their parameters"
                        );
                    }
                }
                _ => {
                    panic!(
                        "`#[tile(kind = recur)]` state+output tiles must return `(RecurState<S>, RecurOutput<T>)` or `RecurControl<(RecurState<S>, RecurOutput<T>)>`"
                    )
                }
            }
        }
    }

    RecurTileShape {
        mode,
        input_param,
        state_param,
        state_inner,
        extra_params,
        output_schema,
    }
}

fn gen_recur_driver_function(
    fn_name: &syn::Ident,
    implementation_name: &syn::Ident,
    shape: &RecurTileShape,
) -> proc_macro2::TokenStream {
    let hidden_name = format_ident!("__raster_recur_auth_{}", fn_name);
    let source_ident = format_ident!("__RasterRecurSource");
    let item_ty =
        recur_input_inner_type(&shape.input_param.ty).expect("validated recur input type");
    let result_ty = shape
        .output_schema
        .as_ref()
        .or(shape.state_inner.as_ref())
        .expect("recur caller-visible result type");
    let result_type_expr = result_ty.to_token_stream().to_string();
    let extra_generic_idents: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, _)| format_ident!("__RasterRecurArg{}", index))
        .collect();
    let extra_trace_idents: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, _)| format_ident!("__raster_recur_trace_arg_{}", index))
        .collect();
    let extra_materialized_idents: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, _)| format_ident!("__raster_recur_materialized_arg_{}", index))
        .collect();
    let extra_wrapper_params: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_generic_idents.iter())
        .map(|(param, generic)| {
            let name = &param.ident;
            quote! { #name: #generic }
        })
        .collect();
    let extra_where: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_generic_idents.iter())
        .map(|(param, generic)| {
            let ty = &param.ty;
            quote! { #generic: ::core::clone::Clone + ::raster::IntoAuthValue<#ty> }
        })
        .collect();
    let extra_arg_trace_defs: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_trace_idents.iter())
        .map(|(param, trace_ident)| {
            let name = &param.ident;
            let ty = &param.ty;
            quote! {
                let #trace_ident = ::raster::into_auth_value::<#ty, _>(#name.clone())
                    .unwrap_or_else(|e| panic!("Failed to trace recur argument '{}': {}", stringify!(#name), e));
            }
        })
        .collect();
    let extra_arg_trace_value_defs: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_trace_idents.iter())
        .map(|(_, trace_ident)| {
            let trace_value_ident = format_ident!("__raster_input_value_{}", trace_ident);
            quote! {
                let #trace_value_ident = if #trace_ident.as_external().is_some() {
                    ::raster::core::trace::FnInputValue::ExternalBinding
                } else if #trace_ident.as_internal().is_some() {
                    ::raster::core::trace::FnInputValue::InternalBinding
                } else {
                    ::raster::core::trace::FnInputValue::Inline(
                        ::raster::core::postcard::to_allocvec(&#trace_ident.clone().into_inner()).unwrap_or_default()
                    )
                };
            }
        })
        .collect();
    let extra_arg_trace_values: Vec<_> = extra_trace_idents
        .iter()
        .map(|trace_ident| {
            let trace_value_ident = format_ident!("__raster_input_value_{}", trace_ident);
            quote! { #trace_value_ident }
        })
        .collect();
    let extra_arg_external_entries: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_trace_idents.iter())
        .map(|(param, trace_ident)| {
            let name_str = param.ident.to_string();
            quote! {
                if let ::core::option::Option::Some(__raster_external_info) = #trace_ident.as_external() {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::ExternalData {
                            name: __raster_external_info.name.clone(),
                            commitment: __raster_external_info
                                .commitment
                                .clone()
                                .map(|value| value.into_bytes())
                                .unwrap_or_default(),
                            tree_root: __raster_external_info.selected.proof.root_hash.clone(),
                            selector: __raster_external_info.selector.clone(),
                            selected: __raster_external_info.selected.clone(),
                        }
                    );
                }
            }
        })
        .collect();
    let extra_arg_internal_entries: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_trace_idents.iter())
        .map(|(param, trace_ident)| {
            let name_str = param.ident.to_string();
            quote! {
                if let ::core::option::Option::Some(__raster_internal_info) = #trace_ident.as_internal() {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::InternalData {
                            coordinates: __raster_internal_info.reference.coordinates.clone(),
                            commitment: __raster_internal_info.reference.commitment.clone(),
                        }
                    );
                }
            }
        })
        .collect();
    let extra_arg_materialization: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_materialized_idents.iter())
        .map(|(param, materialized_ident)| {
            let name = &param.ident;
            let ty = &param.ty;
            quote! {
                let #materialized_ident: #ty = ::raster::into_auth_value::<#ty, _>(#name.clone())
                    .unwrap_or_else(|e| panic!("Failed to materialize auth value for recur argument '{}': {}", stringify!(#name), e))
                    .into_inner();
            }
        })
        .collect();
    let call_expr = match shape.mode {
        RecurTileMode::OutputOnly => quote! {
            {
                #(#extra_arg_materialization)*
                #implementation_name(input, output, #(#extra_materialized_idents),*)
            }
        },
        RecurTileMode::StateOnly => quote! {
            {
                #(#extra_arg_materialization)*
                #implementation_name(input, state, #(#extra_materialized_idents),*)
            }
        },
        RecurTileMode::StateOutput => quote! {
            {
                #(#extra_arg_materialization)*
                #implementation_name(input, state, output, #(#extra_materialized_idents),*)
            }
        },
    };
    let state_wrapper = shape.state_param.as_ref().map(|param| {
        let state_ident = &param.ident;
        let state_ty = &param.ty;
        quote! {
            let #state_ident: #state_ty = ::core::convert::Into::into(#state_ident);
        }
    });
    let state_param = shape.state_param.as_ref().map(|param| {
        let state_ident = &param.ident;
        let state_inner = recur_state_inner_type(&param.ty).expect("validated recur state type");
        quote! { #state_ident: impl ::core::convert::Into<::raster::RecurState<#state_inner>>, }
    });
    let output_param = shape.output_schema.as_ref().map(|output_schema| {
        quote! { output: ::raster::Draft<#output_schema>, }
    });
    let run_driver = match shape.mode {
        RecurTileMode::OutputOnly => {
            let output_schema = shape
                .output_schema
                .as_ref()
                .expect("output-only output schema");
            quote! {
                ::raster::run_recur_list::<#item_ty, #output_schema, _, _>(
                    input,
                    output,
                    move |input, output| #call_expr,
                )
            }
        }
        RecurTileMode::StateOnly => {
            let state_ident = &shape
                .state_param
                .as_ref()
                .expect("state-only state param")
                .ident;
            let state_inner = shape.state_inner.as_ref().expect("state-only state inner");
            quote! {
                ::raster::run_recur_list_state::<#item_ty, #state_inner, _, _>(
                    input,
                    #state_ident,
                    move |input, state| #call_expr,
                )
            }
        }
        RecurTileMode::StateOutput => {
            let state_ident = &shape
                .state_param
                .as_ref()
                .expect("state+output state param")
                .ident;
            let output_schema = shape
                .output_schema
                .as_ref()
                .expect("state+output output schema");
            quote! {
                ::raster::run_recur_list_with_state::<#item_ty, _, #output_schema, _, _>(
                    input,
                    #state_ident,
                    output,
                    move |input, state, output| #call_expr,
                )
            }
        }
    };
    let state_trace_arg = shape.state_param.as_ref().map(|param| {
        let state_ident = &param.ident;
        let state_ty = &param.ty;
        let state_name = state_ident.to_string();
        let state_ty_str = state_ty.to_token_stream().to_string();
        quote! {
            let __raster_input_value_state = ::raster::core::trace::FnInputValue::Inline(
                ::raster::core::postcard::to_allocvec(&#state_ident).unwrap_or_default()
            );
            __raster_input_args.push(::raster::core::trace::FnInputArg {
                name: ::raster::alloc::string::String::from(#state_name),
                ty: ::raster::alloc::string::String::from(#state_ty_str),
            });
            __raster_trace_values.push(__raster_input_value_state.clone());
        }
    });
    let output_trace_arg = shape.output_schema.as_ref().map(|output_schema| {
        quote! {
            let __raster_input_value_output = ::raster::core::trace::FnInputValue::Inline(
                ::raster::core::postcard::to_allocvec(&output).unwrap_or_default()
            );
            __raster_input_args.push(::raster::core::trace::FnInputArg {
                name: ::raster::alloc::string::String::from("output"),
                ty: ::raster::alloc::string::String::from(
                    stringify!(::raster::Draft<#output_schema>)
                ),
            });
            __raster_trace_values.push(__raster_input_value_output.clone());
        }
    });
    let extra_trace_args: Vec<_> = shape
        .extra_params
        .iter()
        .zip(extra_arg_trace_values.iter())
        .map(|(param, trace_value)| {
            let name = param.ident.to_string();
            let ty = param.ty.to_token_stream().to_string();
            quote! {
                __raster_input_args.push(::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(#name),
                    ty: ::raster::alloc::string::String::from(#ty),
                });
                __raster_trace_values.push(#trace_value.clone());
            }
        })
        .collect();
    let wrapper_generics = if extra_generic_idents.is_empty() {
        quote! { <#source_ident> }
    } else {
        quote! { <#source_ident, #(#extra_generic_idents),*> }
    };

    quote! {
        #[doc(hidden)]
        pub fn #hidden_name #wrapper_generics (
            input: #source_ident,
            #state_param
            #output_param
            #(#extra_wrapper_params,)*
        ) -> ::raster::AuthRef<#result_ty>
        where
            #source_ident: ::raster::IntoAuthRef<::raster::alloc::vec::Vec<#item_ty>>,
            #(#extra_where,)*
        {
            let input = ::raster::into_auth_ref::<::raster::alloc::vec::Vec<#item_ty>, _>(input);
            #state_wrapper

            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                let __raster_input_trace = ::raster::auth_ref_trace(&input)
                    .unwrap_or_else(|e| panic!("Failed to trace recur input '{}': {}", stringify!(input), e));
                #(#extra_arg_trace_defs)*
                #(#extra_arg_trace_value_defs)*

                let __raster_input_value_input = __raster_input_trace.value;
                let mut __raster_input_args: ::raster::alloc::vec::Vec<::raster::core::trace::FnInputArg> = ::raster::alloc::vec![
                    ::raster::core::trace::FnInputArg {
                        name: ::raster::alloc::string::String::from("input"),
                        ty: ::raster::alloc::string::String::from(
                            stringify!(::raster::AuthRef<::raster::alloc::vec::Vec<#item_ty>>)
                        ),
                    },
                ];
                let mut __raster_trace_values: ::raster::alloc::vec::Vec<::raster::core::trace::FnInputValue> = ::raster::alloc::vec![
                    __raster_input_value_input.clone(),
                ];
                #output_trace_arg
                #state_trace_arg
                #(#extra_trace_args)*

                let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&__raster_trace_values)
                    .unwrap_or_default();

                let mut __raster_external = ::raster::alloc::collections::BTreeMap::new();
                if let ::core::option::Option::Some(__raster_external_info) = __raster_input_trace.external.as_ref() {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from("input"),
                        __raster_external_info.clone(),
                    );
                }
                #(#extra_arg_external_entries)*

                let mut __raster_internal = ::raster::alloc::collections::BTreeMap::new();
                if let ::core::option::Option::Some(__raster_internal_info) = __raster_input_trace.internal.as_ref() {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from("input"),
                        __raster_internal_info.clone(),
                    );
                }
                #(#extra_arg_internal_entries)*

                let __raster_input = ::core::option::Option::Some(::raster::core::trace::FnInput {
                    data: __raster_input_bytes,
                    values: __raster_trace_values,
                    args: __raster_input_args,
                    external: __raster_external,
                    internal: __raster_internal,
                });

                let __raster_result = #run_driver;
                let __raster_materialized_result: #result_ty =
                    ::raster::materialize_auth_return::<#result_ty, _>(__raster_result.clone());
                let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_materialized_result)
                    .unwrap_or_else(|e| panic!("Failed to serialize recur result: {}", e));

                let __raster_output = ::core::option::Option::Some(::raster::core::trace::FnOutput {
                    data: __raster_output_bytes,
                    ty: ::raster::alloc::string::String::from(#result_type_expr),
                });

                let __raster_record = ::raster::core::trace::FnCallRecord {
                    fn_name: ::raster::alloc::string::String::from(stringify!(#fn_name)),
                    input: __raster_input,
                    output: __raster_output,
                };
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::TileExec(
                    __raster_record,
                ));

                return __raster_result;
            }

            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            {
                #run_driver
            }
        }
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
                if let ::core::option::Option::Some(__raster_external_info) = &#external_info_ident {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::ExternalData {
                            name: __raster_external_info.name.clone(),
                            commitment: __raster_external_info
                                .commitment
                                .clone()
                                .map(|value| value.into_bytes())
                                .unwrap_or_default(),
                            tree_root: __raster_external_info.selected.proof.root_hash.clone(),
                            selector: __raster_external_info.selector.clone(),
                            selected: __raster_external_info.selected.clone(),
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
                if let ::core::option::Option::Some(__raster_internal_info) = &#internal_info_ident {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::InternalData {
                            coordinates: __raster_internal_info.reference.coordinates.clone(),
                            commitment: __raster_internal_info.reference.commitment.clone(),
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

fn is_call_recur_macro(expr_macro: &syn::ExprMacro) -> bool {
    expr_macro.mac.path.is_ident("call_recur")
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

struct RecurCallInput {
    tile: syn::Ident,
    input: Expr,
    state: Option<Expr>,
    output: Option<Expr>,
    args: syn::punctuated::Punctuated<Expr, Token![,]>,
}

fn parse_named_key(input: ParseStream, expected: &str) -> syn::Result<()> {
    let ident: syn::Ident = input.parse()?;
    if ident != expected {
        return Err(syn::Error::new(
            ident.span(),
            format!("expected `{}` key", expected),
        ));
    }
    input.parse::<Token![=]>()?;
    Ok(())
}

impl Parse for RecurCallInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        parse_named_key(input, "tile")?;
        let tile: syn::Ident = input.parse()?;
        input.parse::<Token![,]>()?;

        parse_named_key(input, "input")?;
        let input_expr: Expr = input.parse()?;
        input.parse::<Token![,]>()?;

        let lookahead = input.lookahead1();
        let state = if lookahead.peek(syn::Ident) {
            let fork = input.fork();
            let ident: syn::Ident = fork.parse()?;
            if ident == "state" {
                parse_named_key(input, "state")?;
                let state_expr: Expr = input.parse()?;
                input.parse::<Token![,]>()?;
                Some(state_expr)
            } else {
                None
            }
        } else {
            None
        };

        let output = if input.peek(syn::Ident) {
            let fork = input.fork();
            let ident: syn::Ident = fork.parse()?;
            if ident == "output" {
                parse_named_key(input, "output")?;
                let output_expr: Expr = input.parse()?;
                input.parse::<Token![,]>()?;
                Some(output_expr)
            } else {
                None
            }
        } else {
            None
        };

        if state.is_none() && output.is_none() {
            return Err(syn::Error::new(
                input.span(),
                "call_recur! requires `state = ...` and/or `output = ...` before `args = (...)`",
            ));
        }

        parse_named_key(input, "args")?;
        let content;
        syn::parenthesized!(content in input);
        let args = syn::punctuated::Punctuated::parse_terminated(&content)?;
        let _ = input.parse::<Option<Token![,]>>()?;

        Ok(Self {
            tile,
            input: input_expr,
            state,
            output,
            args,
        })
    }
}

fn rewrite_call_seq_macro(expr_macro: &syn::ExprMacro) -> Expr {
    let input =
        syn::parse2::<SequenceCallInput>(expr_macro.mac.tokens.clone()).unwrap_or_else(|_| {
            panic!("call_seq! expects an identifier callee followed by zero or more arguments")
        });
    let hidden = format_ident!("__raster_sequence_auth_{}", input.callee);
    let args = input.args;
    syn::parse_quote! {
        #hidden(#args)
    }
}

fn rewrite_call_recur_macro(expr_macro: &syn::ExprMacro) -> Expr {
    let input = syn::parse2::<RecurCallInput>(expr_macro.mac.tokens.clone()).unwrap_or_else(|_| {
        panic!(
            "call_recur! expects `tile = ...`, `input = ...`, optional `state = ...`, optional `output = ...`, and `args = (...)`"
        )
    });
    let hidden = format_ident!("__raster_recur_auth_{}", input.tile);
    let input_expr = input.input;
    let state_expr = input.state;
    let output_expr = input.output;
    let args: Vec<_> = input.args.into_iter().collect();
    if let Some(state_expr) = state_expr {
        if let Some(output_expr) = output_expr {
            syn::parse_quote! {
                #hidden(#input_expr, #state_expr, #output_expr #(, #args)*)
            }
        } else {
            syn::parse_quote! {
                #hidden(#input_expr, #state_expr #(, #args)*)
            }
        }
    } else if let Some(output_expr) = output_expr {
        syn::parse_quote! {
            #hidden(#input_expr, #output_expr #(, #args)*)
        }
    } else {
        syn::parse_quote! {
            compile_error!("call_recur! requires `state = ...` and/or `output = ...`")
        }
    }
}

fn rewrite_call_macro(expr_macro: &syn::ExprMacro) -> Expr {
    let input =
        syn::parse2::<SequenceCallInput>(expr_macro.mac.tokens.clone()).unwrap_or_else(|_| {
            panic!("call! expects an identifier callee followed by zero or more arguments")
        });
    let marker = tile_call_binding_marker_ident(&input.callee);
    let original = Expr::Macro(expr_macro.clone());
    syn::parse_quote! {
        ::raster::__private::bind_tile_call::<#marker, _>(#original)
    }
}

fn rewrite_try_call_macro(expr_macro: &syn::ExprMacro) -> Expr {
    let input =
        syn::parse2::<SequenceCallInput>(expr_macro.mac.tokens.clone()).unwrap_or_else(|_| {
            panic!("call! expects an identifier callee followed by zero or more arguments")
        });
    let marker = tile_call_binding_marker_ident(&input.callee);
    let original = Expr::Macro(expr_macro.clone());
    syn::parse_quote! {
        ::raster::__private::bind_tile_try_call::<#marker, _>(#original)
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
                let rewritten = rewrite_call_macro(&expr_macro);
                *stmt = syn::parse_quote! { #rewritten; };
            } else if is_call_seq_macro(&expr_macro) {
                let rewritten = rewrite_call_seq_macro(&expr_macro);
                *stmt = syn::parse_quote! {
                    #rewritten;
                };
            } else if is_call_recur_macro(&expr_macro) {
                let rewritten = rewrite_call_recur_macro(&expr_macro);
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
                *expr = rewrite_call_macro(expr_macro);
            } else if is_call_seq_macro(expr_macro) {
                *expr = rewrite_call_seq_macro(expr_macro);
            } else if is_call_recur_macro(expr_macro) {
                *expr = rewrite_call_recur_macro(expr_macro);
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
                    expr_try.expr = Box::new(rewrite_try_call_macro(expr_macro));
                    return;
                } else if is_call_seq_macro(expr_macro) {
                    expr_try.expr = Box::new(rewrite_call_seq_macro(expr_macro));
                    return;
                } else if is_call_recur_macro(expr_macro) {
                    expr_try.expr = Box::new(rewrite_call_recur_macro(expr_macro));
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
    let return_kind = protocol_return_kind(&input_fn.sig.output);

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_attrs = &input_fn.attrs;
    let fn_body = &input_fn.block;

    let function_wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name_str);
    let implementation_name = format_ident!("__raster_tile_impl_{}", fn_name_str);
    let call_binding_marker = tile_call_binding_marker_ident(fn_name);

    let attrs = TileAttrs::parse(attr);
    let recur_shape = if attrs.tile_type == "recur" {
        Some(validate_recur_tile_shape(&input_fn, &return_kind))
    } else {
        None
    };

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
    let tile_call_binding =
        gen_tile_call_binding_marker(&call_binding_marker, &return_kind, &input_fn.sig.output);
    let recur_driver_function = recur_shape
        .as_ref()
        .map(|shape| gen_recur_driver_function(fn_name, &implementation_name, shape))
        .unwrap_or_else(|| quote! {});

    let mut exposed_sig = input_fn.sig.clone();
    rewrite_into_auth_value_args(&mut exposed_sig);

    let mut implementation_sig = input_fn.sig.clone();
    implementation_sig.ident = implementation_name.clone();

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
        #tile_call_binding
        #recur_driver_function

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
    let draft_trait_ident = format_ident!("{}DraftExt", ident);
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

    let draft_accessor_sigs: Vec<_> = fields
        .iter()
        .map(|field| {
            let field_ident = field.ident.as_ref().expect("named field");
            if let Some(element_ty) = vec_element_type(&field.ty) {
                quote! {
                    fn #field_ident(&mut self) -> ::raster::DraftAppendField<'_, #ident #ty_generics, #element_ty>;
                }
            } else {
                let field_ty = &field.ty;
                quote! {
                    fn #field_ident(&mut self) -> ::raster::DraftSetField<'_, #ident #ty_generics, #field_ty>;
                }
            }
        })
        .collect();

    let draft_accessors: Vec<_> = fields
        .iter()
        .map(|field| {
            let field_ident = field.ident.as_ref().expect("named field");
            let field_name = field_ident.to_string();
            if let Some(element_ty) = vec_element_type(&field.ty) {
                quote! {
                    fn #field_ident(&mut self) -> ::raster::DraftAppendField<'_, #ident #ty_generics, #element_ty> {
                        self.append_field(#field_name)
                    }
                }
            } else {
                let field_ty = &field.ty;
                quote! {
                    fn #field_ident(&mut self) -> ::raster::DraftSetField<'_, #ident #ty_generics, #field_ty> {
                        self.set_field(#field_name)
                    }
                }
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

        pub trait #draft_trait_ident #impl_generics #where_clause {
            #(#draft_accessor_sigs)*
        }

        impl #impl_generics #draft_trait_ident #ty_generics for ::raster::Draft<#ident #ty_generics> #where_clause {
            #(#draft_accessors)*
        }
    })
}
