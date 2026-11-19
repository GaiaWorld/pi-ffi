//! 编译期解析及标准入口的生成实现。

use super::{InvalidExport, PluginDeclaration, RuntimeParameter};
use pi_result::{ErrorKind, ErrorReport};
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens, TokenTree};
use quote::{format_ident, quote};
use std::collections::HashSet;
use syn::{ext::IdentExt, parenthesized, parse::ParseStream, Ident, Token};

// @trace 1
pub(super) fn export_plugin(input: TokenStream) -> TokenStream {
    let tokens = Tokens::from(input);
    let declaration = match syn::parse2::<PluginDeclaration>(tokens.clone()) {
        Ok(declaration) => declaration,
        Err(failure) => return failure.into_compile_error().into(),
    };

    let mut existing = HashSet::new();
    collect_identifiers(tokens, &mut existing);
    let exports = fresh_name("__pi_ffi_exports_v1", &mut existing);
    let metadata = fresh_name("__pi_ffi_metadata_v1", &mut existing);
    let initialize = fresh_name("__pi_ffi_initialize_v1", &mut existing);
    let close = fresh_name("__pi_ffi_close_v1", &mut existing);
    let out = fresh_name("__pi_ffi_out", &mut existing);
    let runtime_ptr = fresh_name("__pi_ffi_runtime", &mut existing);
    let terminal_ptr = fresh_name("__pi_ffi_terminal", &mut existing);
    let config_ptr = fresh_name("__pi_ffi_config", &mut existing);
    let init_id = fresh_name("__pi_ffi_init_id", &mut existing);

    let PluginDeclaration {
        api,
        declaration,
        state,
        initialize: start,
        close: stop,
        runtime,
    } = declaration;
    let initialize_body = match runtime {
        RuntimeParameter::None => quote! {
            #exports.initialize_without_runtime(#runtime_ptr, #terminal_ptr, #config_ptr, #start)
        },
        RuntimeParameter::Optional(runtime) => quote! {
            #exports.initialize::<_, #runtime>(#runtime_ptr, #terminal_ptr, #config_ptr, #start)
        },
    };
    quote! {
        const _: () = {
            static #exports: #api::plugin::Exports<#state> = #api::plugin::Exports::new(#declaration);

            #[unsafe(export_name = "pi_ffi_metadata_v1")]
            unsafe extern "C" fn #metadata(#out: *mut #api::abi::MetadataV1) -> #api::abi::OutcomeV1 {
                unsafe { #exports.metadata(#out) }
            }

            #[unsafe(export_name = "pi_ffi_initialize_v1")]
            unsafe extern "C" fn #initialize(
                #runtime_ptr: *const #api::abi::ObjectRefV1,
                #terminal_ptr: *const #api::abi::ObjectRefV1,
                #config_ptr: *const #api::abi::ObjectRefV1,
            ) -> #api::abi::InitReplyV1 {
                unsafe { #initialize_body }
            }

            #[unsafe(export_name = "pi_ffi_close_v1")]
            unsafe extern "C" fn #close(#init_id: #api::abi::InitIdV1) -> #api::abi::OutcomeV1 {
                unsafe { #exports.close(#init_id, #stop) }
            }

            const _: #api::abi::MetadataFnV1 = #metadata;
            const _: #api::abi::InitializeFnV1 = #initialize;
            const _: #api::abi::CloseFnV1 = #close;
        };
    }.into()
}

// @trace 2
pub(super) fn parse_declaration(input: ParseStream<'_>) -> syn::Result<PluginDeclaration> {
    parse_fields(input).map_err(|failure| {
        let error = ErrorReport::new(InvalidExport {
            message: failure.to_string(),
        })
        .change_context(ErrorKind::InvalidInput);
        syn::Error::new(failure.span(), pi_result::verbose_message(&error))
    })
}

fn parse_fields(input: ParseStream<'_>) -> syn::Result<PluginDeclaration> {
    let mut api = None;
    let mut declaration = None;
    let mut state = None;
    let mut initialize = None;
    let mut close = None;
    let mut runtime = None;

    while !input.is_empty() {
        let key = input.call(Ident::parse_any)?;
        let name = key.unraw().to_string();
        let duplicate = match name.as_str() {
            "api" => api.is_some(),
            "declaration" => declaration.is_some(),
            "state" => state.is_some(),
            "initialize" => initialize.is_some(),
            "close" => close.is_some(),
            "runtime" => runtime.is_some(),
            _ => {
                return Err(syn::Error::new(
                    key.span(),
                    format!("未知字段（unknown field） `{name}`"),
                ))
            }
        };
        if duplicate {
            return Err(syn::Error::new(
                key.span(),
                format!("重复字段（duplicate field） `{name}`"),
            ));
        }

        input.parse::<Token![:]>()?;
        match name.as_str() {
            "api" => api = Some(input.call(syn::Path::parse_mod_style)?),
            "declaration" => declaration = Some(input.parse()?),
            "state" => state = Some(input.parse()?),
            "initialize" => initialize = Some(input.parse()?),
            "close" => close = Some(input.parse()?),
            "runtime" => runtime = Some(parse_runtime(input)?),
            _ => unreachable!("名称（name）已在解析前检查"),
        }
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
    }

    Ok(PluginDeclaration {
        api: api.unwrap_or_else(|| syn::parse_quote!(::pi_ffi)),
        declaration: required(declaration, "declaration")?,
        state: required(state, "state")?,
        initialize: required(initialize, "initialize")?,
        close: required(close, "close")?,
        runtime: required(runtime, "runtime")?,
    })
}

fn parse_runtime(input: ParseStream<'_>) -> syn::Result<RuntimeParameter> {
    let mode = input.call(Ident::parse_any)?;
    match mode.unraw().to_string().as_str() {
        "none" => Ok(RuntimeParameter::None),
        "optional" => {
            let content;
            parenthesized!(content in input);
            let runtime = content.parse()?;
            if !content.is_empty() {
                return Err(content.error("运行时（runtime）只接受一个类型（type）"));
            }
            Ok(RuntimeParameter::Optional(runtime))
        }
        _ => Err(syn::Error::new(
            mode.span(),
            "运行时（runtime）必须使用 none 或 optional(Type)",
        )),
    }
}

fn required<T>(value: Option<T>, field: &str) -> syn::Result<T> {
    value.ok_or_else(|| {
        syn::Error::new(
            Span::call_site(),
            format!("缺少必填字段（missing required field） `{field}`"),
        )
    })
}

fn collect_identifiers(tokens: Tokens, existing: &mut HashSet<String>) {
    for token in tokens {
        match token {
            TokenTree::Ident(ident) => {
                existing.insert(ident.unraw().to_string());
            }
            TokenTree::Group(group) => collect_identifiers(group.stream(), existing),
            _ => {}
        }
    }
}

fn fresh_name(base: &str, existing: &mut HashSet<String>) -> Ident {
    if existing.insert(base.to_owned()) {
        return format_ident!("{base}");
    }
    for suffix in 1u64.. {
        let name = format!("{base}_{suffix}");
        if existing.insert(name.clone()) {
            return format_ident!("{name}");
        }
    }
    unreachable!("标识符（identifier）数量必须小于进程可分配内存")
}

#[cfg(test)]
#[path = "lib_inner_tests.rs"]
mod tests;
