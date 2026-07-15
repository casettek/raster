//! Code generation for `call_recur!` / `#[sequence(kind = recur)]`.
//!
//! A recur site is a loop the schema has to be able to describe: each
//! iteration is its own trace step at its own coordinates, and the site
//! itself is a further step that commits the loop's result. Everything here
//! exists to turn one `call_recur!` into that shape — the shape validation
//! that rejects step functions the protocol cannot express, and the driver
//! that runs the loop while emitting the per-iteration trace events.
//!
//! Split out of `lib.rs` because it is self-contained: the rest of the crate
//! only needs the two validators and the three generators.

use quote::{format_ident, quote, ToTokens};
use syn::{FnArg, ItemFn, ReturnType, Type};

use crate::{
    extract_params, gen_recur_sequence_input_serialization,
    recur_control_inner_type, recur_control_state_output_parts, recur_input_inner_type,
    recur_output_inner_type, recur_sequence_state_inner_type,
    recur_sequence_input_inner_type, recur_sequence_output_inner_type,
    recur_sequence_state_output_parts, recur_state_inner_type, recur_state_output_parts,
    types_equivalent, ParamInfo, ProtocolReturnKind,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecurTileMode {
    OutputOnly,
    StateOnly,
    StateOutput,
}

#[derive(Clone)]
pub(crate) struct RecurTileShape {
    mode: RecurTileMode,
    input_param: ParamInfo,
    state_param: Option<ParamInfo>,
    state_inner: Option<Type>,
    extra_params: Vec<ParamInfo>,
    output_schema: Option<Type>,
}

#[derive(Clone)]
pub(crate) struct RecurSequenceShape {
    mode: RecurTileMode,
    input_param: ParamInfo,
    state_param: Option<ParamInfo>,
    state_inner: Option<Type>,
    extra_params: Vec<ParamInfo>,
    output_schema: Option<Type>,
}

pub(crate) fn validate_recur_tile_shape(input: &ItemFn, return_kind: &ProtocolReturnKind) -> RecurTileShape {
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

pub(crate) fn validate_recur_sequence_shape(input: &ItemFn) -> RecurSequenceShape {
    let params = extract_params(input);
    if params.len() < 2 {
        panic!("`#[sequence(kind = recur)]` functions must accept at least `(input, state)` or `(input, output)`");
    }

    let input_param = params.first().cloned().expect("recur sequence input param");
    recur_sequence_input_inner_type(&input_param.ty).unwrap_or_else(|| {
        panic!(
            "`#[sequence(kind = recur)]` functions must start with `input: RecurSequenceInput<T>`"
        )
    });

    let second_param = params.get(1).cloned().expect("recur sequence second param");
    let (mode, state_param, state_inner, output_param, output_schema, extra_start) = if let Some(
        output_schema,
    ) =
        recur_sequence_output_inner_type(&second_param.ty)
    {
        if params
            .get(2)
            .is_some_and(|param| recur_sequence_state_inner_type(&param.ty).is_some())
        {
            panic!("`#[sequence(kind = recur)]` functions must place `state: RecurSequenceState<S>` before `output: RecurSequenceOutput<T>`");
        }
        (
            RecurTileMode::OutputOnly,
            None,
            None,
            Some(second_param),
            Some(output_schema),
            2usize,
        )
    } else if let Some(state_inner) = recur_sequence_state_inner_type(&second_param.ty) {
        if let Some(third_param) = params.get(2).cloned() {
            if let Some(output_schema) = recur_sequence_output_inner_type(&third_param.ty) {
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
        panic!("`#[sequence(kind = recur)]` functions must place `state: RecurSequenceState<S>` or `output: RecurSequenceOutput<T>` after `input`");
    };

    let extra_params = params[extra_start..].to_vec();
    if extra_params.iter().any(|param| {
        recur_sequence_state_inner_type(&param.ty).is_some()
            || recur_sequence_output_inner_type(&param.ty).is_some()
            || recur_sequence_input_inner_type(&param.ty).is_some()
            || recur_state_inner_type(&param.ty).is_some()
            || recur_output_inner_type(&param.ty).is_some()
            || recur_input_inner_type(&param.ty).is_some()
    }) {
        panic!("`#[sequence(kind = recur)]` only supports plain `args...` after recur sequence state/output parameters");
    }

    let ReturnType::Type(_, return_ty) = &input.sig.output else {
        panic!("`#[sequence(kind = recur)]` functions must return threaded recur sequence state/output handles");
    };
    if recur_control_inner_type(return_ty).is_some() {
        panic!("`#[sequence(kind = recur)]` cannot return `RecurControl`; early termination must be decided inside recur tiles");
    }

    match mode {
        RecurTileMode::OutputOnly => {
            let output_param = output_param
                .as_ref()
                .expect("output-only recur sequence output param");
            if !types_equivalent(return_ty, &output_param.ty) {
                panic!("`#[sequence(kind = recur)]` output-only functions must return the same `RecurSequenceOutput<S>` type as their output parameter");
            }
        }
        RecurTileMode::StateOnly => {
            let state_param = state_param
                .as_ref()
                .expect("state-only recur sequence state param");
            if !types_equivalent(return_ty, &state_param.ty) {
                panic!("`#[sequence(kind = recur)]` state-only functions must return the same `RecurSequenceState<S>` type as their state parameter");
            }
        }
        RecurTileMode::StateOutput => {
            let state_param = state_param
                .as_ref()
                .expect("state+output recur sequence state param");
            let output_param = output_param
                .as_ref()
                .expect("state+output recur sequence output param");
            let (return_state_ty, return_output_ty) =
                recur_sequence_state_output_parts(return_ty).unwrap_or_else(|| {
                    panic!("`#[sequence(kind = recur)]` state+output functions must return `(RecurSequenceState<S>, RecurSequenceOutput<T>)` matching their parameters")
                });
            let state_inner = recur_sequence_state_inner_type(&state_param.ty)
                .expect("recur sequence state inner type");
            if !types_equivalent(&return_state_ty, &state_inner)
                || !types_equivalent(&return_output_ty, &output_param.ty)
            {
                panic!("`#[sequence(kind = recur)]` state+output functions must return `(RecurSequenceState<S>, RecurSequenceOutput<T>)` matching their parameters");
            }
        }
    }

    RecurSequenceShape {
        mode,
        input_param,
        state_param,
        state_inner,
        extra_params,
        output_schema,
    }
}

pub(crate) fn gen_recur_driver_function(
    fn_name: &syn::Ident,
    shape: &RecurTileShape,
) -> proc_macro2::TokenStream {
    let fn_name_str = fn_name.to_string();
    let hidden_name = format_ident!("__raster_recur_auth_{}", fn_name);
    let source_ident = format_ident!("__RasterRecurSource");
    let item_ty =
        recur_input_inner_type(&shape.input_param.ty).expect("validated recur input type");
    let result_ty = shape
        .output_schema
        .as_ref()
        .or(shape.state_inner.as_ref())
        .expect("recur caller-visible result type");
    let extra_generic_idents: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, _)| format_ident!("__RasterRecurArg{}", index))
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
                #fn_name(input, output, #(#extra_materialized_idents),*)
            }
        },
        RecurTileMode::StateOnly => quote! {
            {
                #(#extra_arg_materialization)*
                #fn_name(input, state, #(#extra_materialized_idents),*)
            }
        },
        RecurTileMode::StateOutput => quote! {
            {
                #(#extra_arg_materialization)*
                #fn_name(input, state, output, #(#extra_materialized_idents),*)
            }
        },
    };
    let state_wrapper = shape.state_param.as_ref().map(|param| {
        let state_ident = &param.ident;
        let state_inner = recur_state_inner_type(&param.ty).expect("validated recur state type");
        quote! {
            let #state_ident: ::raster::RecurState<#state_inner> =
                ::core::convert::Into::into(#state_ident);
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
    let wrapper_generics = if extra_generic_idents.is_empty() {
        quote! { <#source_ident> }
    } else {
        quote! { <#source_ident, #(#extra_generic_idents),*> }
    };
    let state_trace_capture = shape
        .state_param
        .as_ref()
        .map(|param| {
            let state_ident = &param.ident;
            let state_ty = &param.ty;
            let state_ty_str = state_ty.to_token_stream().to_string();
            quote! {
                __raster_trace_values.push(::raster::core::trace::FnInputValue::Inline(
                    ::raster::core::postcard::to_allocvec(&#state_ident).unwrap_or_default()
                ));
                __raster_trace_args.push(::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(stringify!(#state_ident)),
                    ty: ::raster::alloc::string::String::from(#state_ty_str),
                });
            }
        })
        .unwrap_or_else(|| quote! {});
    let output_trace_capture = shape
        .output_schema
        .as_ref()
        .map(|output_schema| {
            let output_ty = quote! { ::raster::Draft<#output_schema> }.to_string();
            quote! {
                __raster_trace_values.push(::raster::core::trace::FnInputValue::Inline(
                    ::raster::serialize_draft_replay_handle::<#output_schema>(&output)
                ));
                __raster_trace_args.push(::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from("output"),
                    ty: ::raster::alloc::string::String::from(#output_ty),
                });
            }
        })
        .unwrap_or_else(|| quote! {});
    let extra_trace_capture: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let trace_ident = format_ident!("__raster_recur_trace_arg_{}", index);
            let name = &param.ident;
            let name_str = name.to_string();
            let ty = &param.ty;
            let ty_str = ty.to_token_stream().to_string();
            quote! {
                let #trace_ident = ::raster::into_auth_value::<#ty, _>(#name.clone())
                    .unwrap_or_else(|e| panic!("Failed to materialize auth value for recur argument '{}': {}", stringify!(#name), e));
                let __raster_trace_value = match #trace_ident {
                    ::raster::AuthValue::Internal(__raster_internal_value) => {
                        __raster_internal.insert(
                            ::raster::alloc::string::String::from(#name_str),
                            ::raster::core::trace::InternalData {
                                coordinates: __raster_internal_value.reference.coordinates.clone(),
                                commitment: __raster_internal_value.reference.commitment.clone(),
                                selector: __raster_internal_value.selector.clone(),
                                selection: __raster_internal_value.selection.clone(),
                            }
                        );
                        ::raster::core::trace::FnInputValue::InternalBinding
                    }
                    ::raster::AuthValue::Inline(__raster_inline_value) => {
                        ::raster::core::trace::FnInputValue::Inline(
                            ::raster::core::postcard::to_allocvec(&__raster_inline_value)
                                .unwrap_or_default()
                        )
                    }
                };
                __raster_trace_values.push(__raster_trace_value);
                __raster_trace_args.push(::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(#name_str),
                    ty: ::raster::alloc::string::String::from(#ty_str),
                });
            }
        })
        .collect();

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
                    .unwrap_or_else(|e| panic!("Failed to build recur input trace: {}", e));
                let mut __raster_trace_values = ::raster::alloc::vec::Vec::new();
                let mut __raster_trace_args = ::raster::alloc::vec::Vec::new();
                let mut __raster_internal = ::raster::alloc::collections::BTreeMap::new();

                __raster_trace_values.push(__raster_input_trace.value);
                __raster_trace_args.push(::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from("input"),
                    ty: ::raster::alloc::string::String::from(stringify!(::raster::AuthRef<::raster::alloc::vec::Vec<#item_ty>>)),
                });
                if let ::core::option::Option::Some(__raster_internal_info) = __raster_input_trace.internal {
                    __raster_internal.insert(
                        ::raster::alloc::string::String::from("input"),
                        __raster_internal_info,
                    );
                }
                #state_trace_capture
                #output_trace_capture
                #(#extra_trace_capture)*
                let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&(
                    __raster_trace_values.clone(),
                    __raster_internal.clone(),
                ))
                .unwrap_or_default();
                let __raster_input = ::core::option::Option::Some(::raster::core::trace::FnInput {
                    data: __raster_input_bytes,
                    values: __raster_trace_values,
                    args: __raster_trace_args,
                    internal: __raster_internal,
                });

                let __raster_recur_trace_scope = ::raster::__private::RecurTraceScopeGuard::enter();
                let result = #run_driver;
                drop(__raster_recur_trace_scope);

                let __raster_resolved_output = ::raster::resolve_internal_value::<#result_ty>(result.reference().clone())
                    .unwrap_or_else(|e| panic!("Failed to resolve recur output for trace: {}", e));
                let __raster_output_bytes = __raster_resolved_output.bytes.clone();
                let __raster_output = ::core::option::Option::Some(
                    ::raster::core::trace::FnOutput::new(
                        __raster_output_bytes,
                        stringify!(::raster::AuthRef<#result_ty>),
                    ).with_raster(
                        ::raster::raster_trace_payload(&__raster_resolved_output.value)
                            .unwrap_or_else(|e| panic!("Failed to build raster recur output payload: {}", e))
                    )
                );
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::RecurTileExec(
                    ::raster::core::trace::FnCallRecord {
                        fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                        input: __raster_input,
                        output: __raster_output,
                        draft_transition_witness: ::core::option::Option::None,
                    }
                ));

                return result;
            }

            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            {
                #run_driver
            }
        }
    }
}

pub(crate) fn gen_recur_sequence_step_function(
    fn_name: &syn::Ident,
    item_fn: &ItemFn,
    shape: &RecurSequenceShape,
) -> proc_macro2::TokenStream {
    let fn_name_str = fn_name.to_string();
    let step_name = format_ident!("__raster_recur_sequence_step_{}", fn_name);
    let mut step_sig = item_fn.sig.clone();
    step_sig.ident = step_name;
    for arg in step_sig.inputs.iter_mut() {
        let FnArg::Typed(pat_type) = arg else {
            continue;
        };
        let syn::Pat::Ident(pat_ident) = pat_type.pat.as_ref() else {
            continue;
        };
        if let Some(param) = shape
            .extra_params
            .iter()
            .find(|param| param.ident == pat_ident.ident)
        {
            let ty = &param.ty;
            pat_type.ty = syn::parse_quote!(::raster::AuthRef<#ty>);
        }
    }
    let body = &item_fn.block;
    let input_serialization = gen_recur_sequence_input_serialization(item_fn);
    let output_type_expr = match &item_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };
    let result_binding = match shape.mode {
        RecurTileMode::OutputOnly => {
            let output_schema = shape
                .output_schema
                .as_ref()
                .expect("output-only recur sequence output schema");
            quote! {
                let result: ::raster::RecurSequenceOutput<#output_schema> =
                    ::core::convert::Into::into((|| #body)());
            }
        }
        RecurTileMode::StateOnly => {
            let state_inner = shape
                .state_inner
                .as_ref()
                .expect("state-only recur sequence state inner");
            quote! {
                let result: ::raster::RecurSequenceState<#state_inner> =
                    ::core::convert::Into::into((|| #body)());
            }
        }
        RecurTileMode::StateOutput => {
            let state_inner = shape
                .state_inner
                .as_ref()
                .expect("state+output recur sequence state inner");
            let output_schema = shape
                .output_schema
                .as_ref()
                .expect("state+output recur sequence output schema");
            quote! {
                let (__raster_recur_sequence_state_result, __raster_recur_sequence_output_result) =
                    (|| #body)();
                let result: (
                    ::raster::RecurSequenceState<#state_inner>,
                    ::raster::RecurSequenceOutput<#output_schema>,
                ) = (
                    ::core::convert::Into::into(__raster_recur_sequence_state_result),
                    ::core::convert::Into::into(__raster_recur_sequence_output_result),
                );
            }
        }
    };

    quote! {
        #[doc(hidden)]
        pub #step_sig {
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                let __raster_recur_sequence_iteration_scope =
                    ::raster::__private::RecurSequenceIterationScopeGuard::enter();
                #input_serialization
                let mut __raster_record = ::raster::core::trace::FnCallRecord {
                    fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                    input: __raster_input,
                    output: ::core::option::Option::None,
                    draft_transition_witness: ::core::option::Option::None,
                };
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::RecurSequenceStart(
                    __raster_record.clone(),
                ));
                #result_binding
                let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&result)
                    .unwrap_or_default();
                __raster_record.output = ::core::option::Option::Some(
                    ::raster::core::trace::FnOutput::new(
                        __raster_output_bytes,
                        ::raster::alloc::string::String::from(#output_type_expr),
                    )
                );
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::RecurSequenceEnd(
                    __raster_record,
                ));
                let _ = &__raster_recur_sequence_iteration_scope;
                result
            }

            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            {
                #result_binding
                result
            }
        }
    }
}

pub(crate) fn gen_recur_sequence_driver_function(
    fn_name: &syn::Ident,
    shape: &RecurSequenceShape,
) -> proc_macro2::TokenStream {
    let fn_name_str = fn_name.to_string();
    let hidden_name = format_ident!("__raster_recur_sequence_auth_{}", fn_name);
    let step_name = format_ident!("__raster_recur_sequence_step_{}", fn_name);
    let source_ident = format_ident!("__RasterRecurSequenceSource");
    let item_ty = recur_sequence_input_inner_type(&shape.input_param.ty)
        .expect("validated recur sequence input type");
    let result_ty = shape
        .output_schema
        .as_ref()
        .or(shape.state_inner.as_ref())
        .expect("recur sequence caller-visible result type");
    let extra_generic_idents: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, _)| format_ident!("__RasterRecurSequenceArg{}", index))
        .collect();
    let extra_materialized_idents: Vec<_> = shape
        .extra_params
        .iter()
        .enumerate()
        .map(|(index, _)| format_ident!("__raster_recur_sequence_materialized_arg_{}", index))
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
            quote! { #generic: ::core::clone::Clone + ::raster::IntoAuthRef<#ty> }
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
                let #materialized_ident: ::raster::AuthRef<#ty> =
                    ::raster::into_auth_ref::<#ty, _>(#name.clone());
            }
        })
        .collect();
    let call_expr = match shape.mode {
        RecurTileMode::OutputOnly => quote! {
            {
                #(#extra_arg_materialization)*
                #step_name(input, output, #(#extra_materialized_idents),*)
            }
        },
        RecurTileMode::StateOnly => quote! {
            {
                #(#extra_arg_materialization)*
                #step_name(input, state, #(#extra_materialized_idents),*)
            }
        },
        RecurTileMode::StateOutput => quote! {
            {
                #(#extra_arg_materialization)*
                #step_name(input, state, output, #(#extra_materialized_idents),*)
            }
        },
    };
    let state_wrapper = shape.state_param.as_ref().map(|param| {
        let state_ident = &param.ident;
        let state_inner = recur_sequence_state_inner_type(&param.ty)
            .expect("validated recur sequence state type");
        quote! {
            let #state_ident: ::raster::RecurState<#state_inner> =
                ::core::convert::Into::into(#state_ident);
        }
    });
    let state_param = shape.state_param.as_ref().map(|param| {
        let state_ident = &param.ident;
        let state_inner = recur_sequence_state_inner_type(&param.ty)
            .expect("validated recur sequence state type");
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
                ::raster::run_recur_sequence_list::<#item_ty, #output_schema, _, _>(
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
                ::raster::run_recur_sequence_list_state::<#item_ty, #state_inner, _, _>(
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
                ::raster::run_recur_sequence_list_with_state::<#item_ty, _, #output_schema, _, _>(
                    input,
                    #state_ident,
                    output,
                    move |input, state, output| #call_expr,
                )
            }
        }
    };
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
                    .unwrap_or_else(|e| panic!("Failed to build recur sequence input trace: {}", e));
                let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&__raster_input_trace.value)
                    .unwrap_or_default();
                let __raster_input = ::core::option::Option::Some(::raster::core::trace::FnInput {
                    data: __raster_input_bytes,
                    values: ::raster::alloc::vec![__raster_input_trace.value],
                    args: ::raster::alloc::vec![::raster::core::trace::FnInputArg {
                        name: ::raster::alloc::string::String::from("input"),
                        ty: ::raster::alloc::string::String::from(stringify!(::raster::AuthRef<::raster::alloc::vec::Vec<#item_ty>>)),
                    }],
                    internal: __raster_input_trace.internal.map(|internal| {
                        let mut map = ::raster::alloc::collections::BTreeMap::new();
                        map.insert(::raster::alloc::string::String::from("input"), internal);
                        map
                    }).unwrap_or_default(),
                });

                let result = #run_driver;

                let __raster_resolved_output = ::raster::resolve_internal_value::<#result_ty>(result.reference().clone())
                    .unwrap_or_else(|e| panic!("Failed to resolve recur sequence output for trace: {}", e));
                let __raster_output_bytes = __raster_resolved_output.bytes.clone();
                let __raster_output = ::core::option::Option::Some(
                    ::raster::core::trace::FnOutput::new(
                        __raster_output_bytes,
                        stringify!(::raster::AuthRef<#result_ty>),
                    ).with_raster(
                        ::raster::raster_trace_payload(&__raster_resolved_output.value)
                            .unwrap_or_else(|e| panic!("Failed to build raster recur sequence output payload: {}", e))
                    )
                );
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::RecurSequenceExec(
                    ::raster::core::trace::FnCallRecord {
                        fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                        input: __raster_input,
                        output: __raster_output,
                        draft_transition_witness: ::core::option::Option::None,
                    }
                ));

                return result;
            }

            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            {
                #run_driver
            }
        }
    }
}

