//! 编译期解析及标准入口的生成实现。

use super::{InitializationCallbacks, InvalidExport, PluginDeclaration, RuntimeParameter};
use pi_ffi_abi::management::{InterfaceVersion, InterfaceVersionStatus, ManagementInterface, INTERFACE_VERSIONS, INTERFACE_VERSION_CATALOG_REVISION};
use pi_result::{ErrorKind, ErrorReport};
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens, TokenTree};
use quote::{format_ident, quote};
use std::collections::HashSet;
use syn::{braced, bracketed, ext::IdentExt, parenthesized, parse::ParseStream, Ident, Token};

// @trace 1
pub(super) fn export_plugin(input: TokenStream) -> TokenStream {
    match expand(Tokens::from(input)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

// 使用原输入标记（tokens）保持标识卫生，普通单元测试亦经过同一真实展开出口。
fn expand(tokens: Tokens) -> syn::Result<Tokens> {
    let parsed = syn::parse2::<PluginDeclaration>(tokens.clone())?;
    let selected = select_versions(parsed.versions.as_deref(), INTERFACE_VERSIONS)?;
    let callbacks = initialization_callbacks(&parsed.initialize, &selected)?;
    let mut existing = HashSet::new();
    collect_identifiers(tokens, &mut existing);
    let exports = fresh_name("__pi_ffi_exports_v1", &mut existing);
    let out = fresh_name("__pi_ffi_out", &mut existing);
    let runtime_ptr = fresh_name("__pi_ffi_runtime", &mut existing);
    let terminal_ptr = fresh_name("__pi_ffi_terminal", &mut existing);
    let config_ptr = fresh_name("__pi_ffi_config", &mut existing);
    let data_ptr = fresh_name("__pi_ffi_data", &mut existing);
    let init_id = fresh_name("__pi_ffi_init_id", &mut existing);
    let api = &parsed.api;
    let declaration = &parsed.declaration;
    let state = &parsed.state;
    let stop = &parsed.close;
    let mut wrappers = Tokens::new();
    for version in selected {
        let symbol = version.symbol.to_str().expect("目录符号（catalog symbol）为 UTF-8");
        let function = fresh_name(&format!("__{symbol}"), &mut existing);
        match version.interface {
            ManagementInterface::Metadata => wrappers.extend(quote! {
                #[unsafe(export_name = #symbol)]
                unsafe extern "C" fn #function(#out: *mut #api::abi::MetadataV1) -> #api::abi::OutcomeV1 {
                    unsafe { #exports.metadata(#out) }
                }
                const _: #api::abi::MetadataFnV1 = #function;
            }),
            ManagementInterface::Close => wrappers.extend(quote! {
                #[unsafe(export_name = #symbol)]
                unsafe extern "C" fn #function(#init_id: #api::abi::InitIdV1) -> #api::abi::OutcomeV1 {
                    unsafe { #exports.close(#init_id, #stop) }
                }
                const _: #api::abi::CloseFnV1 = #function;
            }),
            ManagementInterface::Initialization => {
                let start = callbacks.iter().find(|(major, _)| *major == version.major).expect("每版已有回调（callback）").1;
                let (body, fourth, signature) = match version.major {
                    1 => {
                        let body = match &parsed.runtime {
                            RuntimeParameter::None => quote!(#exports.initialize_v1_without_runtime(#runtime_ptr, #terminal_ptr, #config_ptr, #start)),
                            RuntimeParameter::Optional(runtime) => quote!(#exports.initialize_v1::<_, #runtime>(#runtime_ptr, #terminal_ptr, #config_ptr, #start)),
                        };
                        (body, Tokens::new(), quote!(#api::abi::InitializeFnV1))
                    }
                    2 => {
                        let body = match &parsed.runtime {
                            RuntimeParameter::None => quote!(#exports.initialize_without_runtime(#runtime_ptr, #terminal_ptr, #config_ptr, #data_ptr, #start)),
                            RuntimeParameter::Optional(runtime) => quote!(#exports.initialize::<_, #runtime, _>(#runtime_ptr, #terminal_ptr, #config_ptr, #data_ptr, #start)),
                        };
                        (body, quote!(#data_ptr: *const #api::abi::ObjectRefV1,), quote!(#api::abi::InitializeFnV2))
                    }
                    _ => return Err(invalid(Span::call_site(), "初始化规范（initialization version）没有准确接收方法")),
                };
                wrappers.extend(quote! {
                    #[unsafe(export_name = #symbol)]
                    unsafe extern "C" fn #function(
                        #runtime_ptr: *const #api::abi::ObjectRefV1,
                        #terminal_ptr: *const #api::abi::ObjectRefV1,
                        #config_ptr: *const #api::abi::ObjectRefV1,
                        #fourth
                    ) -> #api::abi::InitReplyV1 { unsafe { #body } }
                    const _: #signature = #function;
                });
            }
        }
    }
    let revision = INTERFACE_VERSION_CATALOG_REVISION;
    Ok(quote! {
        const _: () = {
            const _: () = assert!(#api::abi::INTERFACE_VERSION_CATALOG_REVISION == #revision, "接口目录修订（interface catalog revision）不匹配");
            static #exports: #api::plugin::Exports<#state> = #api::plugin::Exports::new(#declaration);
            #wrappers
        };
    })
}

fn invalid(span: Span, message: &str) -> syn::Error {
    let error = ErrorReport::new(InvalidExport { message: message.into() }).change_context(ErrorKind::InvalidInput);
    syn::Error::new(span, pi_result::verbose_message(&error))
}

fn select_versions<'a>(requested: Option<&[Ident]>, catalog: &'a [InterfaceVersion]) -> syn::Result<Vec<&'a InterfaceVersion>> {
    let mut selected = Vec::new();
    match requested {
        None => selected.extend(catalog.iter().filter(|entry| entry.latest && entry.status == InterfaceVersionStatus::Available)),
        Some(requested) => {
            for name in requested {
                let text = name.unraw().to_string();
                if text == "all_legacy" {
                    selected.extend(catalog.iter().filter(|entry| !entry.latest && entry.status == InterfaceVersionStatus::Available));
                } else {
                    let entry = catalog.iter().find(|entry| entry.feature == text).ok_or_else(|| invalid(name.span(), &format!("未知接口版本（unknown interface version） `{text}`")))?;
                    if entry.status == InterfaceVersionStatus::Yanked {
                        return Err(invalid(name.span(), &format!("接口版本（interface version） `{text}` 已撤回（yanked）")));
                    }
                    selected.push(entry);
                }
            }
        }
    }
    // 顺序由完整目录（catalog）唯一确定，重叠选择没有额外效果。
    let selected = catalog.iter().filter(|entry| selected.iter().any(|known| known.feature == entry.feature)).collect::<Vec<_>>();
    for interface in [ManagementInterface::Initialization, ManagementInterface::Metadata, ManagementInterface::Close] {
        if !selected.iter().any(|entry| entry.interface == interface) {
            return Err(invalid(Span::call_site(), &format!("缺少管理接口类别（missing management interface） {interface:?}")));
        }
    }
    Ok(selected)
}

fn initialization_callbacks<'a>(callbacks: &'a InitializationCallbacks, selected: &[&InterfaceVersion]) -> syn::Result<Vec<(u32, &'a syn::Path)>> {
    let versions = selected.iter().filter(|entry| entry.interface == ManagementInterface::Initialization).collect::<Vec<_>>();
    match callbacks {
        InitializationCallbacks::Single(path) if versions.len() == 1 => Ok(vec![(versions[0].major, path)]),
        InitializationCallbacks::Single(_) => Err(invalid(Span::call_site(), "多版初始化（initialization）必须按版本指定回调（callback）")),
        InitializationCallbacks::Versions(callbacks) => {
            let mut result = Vec::new();
            for (name, path) in callbacks {
                let text = name.unraw().to_string();
                let version = versions.iter().find(|entry| entry.feature == text).ok_or_else(|| invalid(name.span(), &format!("未选择的初始化回调（initialization callback） `{text}`")))?;
                result.push((version.major, path));
            }
            if versions.iter().any(|version| !result.iter().any(|(major, _)| *major == version.major)) {
                return Err(invalid(Span::call_site(), "缺少所选初始化版本的回调（initialization callback）"));
            }
            Ok(result)
        }
    }
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
    let mut versions = None;

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
            "versions" => versions.is_some(),
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
            "initialize" => initialize = Some(parse_callbacks(input)?),
            "versions" => {
                let content;
                bracketed!(content in input);
                versions = Some(content.parse_terminated(Ident::parse_any, Token![,])?.into_iter().collect());
            }
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
        versions,
        initialize: required(initialize, "initialize")?,
        close: required(close, "close")?,
        runtime: required(runtime, "runtime")?,
    })
}

fn parse_callbacks(input: ParseStream<'_>) -> syn::Result<InitializationCallbacks> {
    if !input.peek(syn::token::Brace) {
        return Ok(InitializationCallbacks::Single(input.parse()?));
    }
    let content;
    braced!(content in input);
    let mut callbacks = Vec::new();
    let mut seen = HashSet::new();
    while !content.is_empty() {
        let name = content.call(Ident::parse_any)?;
        if !seen.insert(name.unraw().to_string()) {
            return Err(syn::Error::new(name.span(), format!("重复初始化回调（duplicate initialization callback） `{name}`")));
        }
        content.parse::<Token![:]>()?;
        callbacks.push((name, content.parse()?));
        if !content.is_empty() { content.parse::<Token![,]>()?; }
    }
    Ok(InitializationCallbacks::Versions(callbacks))
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
