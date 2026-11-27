use super::PluginDeclaration;
use crate::RuntimeParameter;
use quote::quote;

/// 核具名宏输入（named macro input）保留 Rust 路径、复杂类型与尾逗号。
#[test]
fn test_parse_explicit_api_and_optional_type() {
    let parsed = syn::parse_str::<PluginDeclaration>(
        "runtime: optional(::clock::Handle<(u32, u64)>), state: State, close: stop, \
         api: ::sdk, initialize: r#start, declaration: detail::DECLARATION,",
    )
    .unwrap();
    let PluginDeclaration {
        api,
        declaration,
        state,
        initialize,
        close,
        runtime,
        versions,
    } = parsed;
    assert!(versions.is_none());
    let crate::InitializationCallbacks::Single(initialize) = initialize else { panic!("单回调（single callback）"); };
    assert_eq!(quote!(#api).to_string(), quote!(::sdk).to_string());
    assert_eq!(
        quote!(#declaration).to_string(),
        quote!(detail::DECLARATION).to_string()
    );
    assert_eq!(quote!(#state).to_string(), "State");
    assert_eq!(quote!(#initialize).to_string(), "r#start");
    assert_eq!(quote!(#close).to_string(), "stop");
    let RuntimeParameter::Optional(runtime) = runtime else {
        panic!("expected optional runtime");
    };
    assert_eq!(
        quote!(#runtime).to_string(),
        quote!(::clock::Handle<(u32, u64)>).to_string()
    );
}

/// 省略 api 的宏（macro）使用绝对默认路径，none 表示实际初始化不接收运行时。
#[test]
fn test_parse_default_api_and_absent_runtime() {
    let parsed = syn::parse_str::<PluginDeclaration>(
        "declaration: DECLARATION, state: State, initialize: start, close: stop, runtime: none",
    )
    .unwrap();
    let api = parsed.api;
    assert_eq!(quote!(#api).to_string(), quote!(::pi_ffi).to_string());
    assert!(matches!(parsed.runtime, RuntimeParameter::None));
}

/// 原字段和值的解析（parse）失败保留实际信息，并进入 pi_result 的输入分类。
#[test]
fn test_parse_reports_duplicate_unknown_and_missing_fields() {
    let cases = [
        ("api: sdk, r#api: shadow", "重复字段"),
        ("bad: state", "未知字段"),
        ("declaration: DECLARATION", "缺少必填字段"),
        ("runtime: optional(u32, u64)", "只接受一个类型"),
        ("runtime: required", "必须使用 none 或 optional"),
    ];
    for (input, reason) in cases {
        let error = syn::parse_str::<PluginDeclaration>(input).err().unwrap();
        let message = error.to_string();
        assert!(message.contains("invalid input"), "{message}");
        assert!(message.contains(reason), "{message}");
    }
}

/// 显式多版本声明（declaration）必须可解析，各版保留独立回调（callback）。
#[test]
fn compatibility_parses_explicit_versions_and_callbacks() {
    let result = syn::parse_str::<PluginDeclaration>(
        "declaration: D, state: S, versions: [metadata_v1, init_v1, init_v2, close_v1], initialize: { init_v1: old, init_v2: new }, close: stop, runtime: none",
    );
    assert!(result.is_ok(), "{}", result.err().map(|e| e.to_string()).unwrap_or_default());
}

/// 汇总选择（aggregate selection）和显式重叠只产生一组准确导出，共享一份状态。
#[test]
fn compatibility_expansion_deduplicates_and_preserves_exact_versions() {
    for runtime in ["none", "optional(::clock::Handle)"] {
        let source = format!("api: ::sdk, declaration: D, state: S, versions: [metadata_v1, init_v2, all_legacy, init_v1, init_v2, close_v1], initialize: {{init_v1: old, init_v2: new}}, close: stop, runtime: {runtime}");
        let generated = super::expand(source.parse().unwrap()).unwrap().to_string();
        for symbol in ["pi_ffi_metadata_v1", "pi_ffi_initialize_v1", "pi_ffi_initialize_v2", "pi_ffi_close_v1"] {
            assert_eq!(generated.matches(&format!("\"{symbol}\"")).count(), 1, "{generated}");
        }
        assert_eq!(generated.matches("static ").count(), 1);
        assert!(generated.contains("InitializeFnV1"));
        assert!(generated.contains("InitializeFnV2"));
        assert!(generated.contains("INTERFACE_VERSION_CATALOG_REVISION"));
        assert!(!generated.contains("cfg"));
    }
    let latest = super::expand("declaration: D, state: S, initialize: new, close: stop, runtime: none".parse().unwrap()).unwrap().to_string();
    assert!(!latest.contains("\"pi_ffi_initialize_v1\""));
    assert_eq!(latest.matches("export_name").count(), 3);
}

/// 非法版本集合（version set）及不完整回调（callback）不能生成静默替代。
#[test]
fn compatibility_rejects_invalid_sets_and_callback_maps() {
    let cases = [
        ("versions: [metadata_v1, init_v9, close_v1], initialize: start", "未知接口版本"),
        ("versions: [init_v1], initialize: start", "缺少管理接口类别"),
        ("versions: [metadata_v1, init_v1, init_v2, close_v1], initialize: start", "必须按版本"),
        ("versions: [metadata_v1, init_v1, init_v2, close_v1], initialize: {init_v1: old}", "缺少所选"),
        ("versions: [metadata_v1, init_v2, close_v1], initialize: {init_v1: old, init_v2: new}", "未选择"),
        ("versions: [metadata_v1, init_v1, close_v1], initialize: {init_v1: old, r#init_v1: duplicate}", "重复初始化回调"),
    ];
    for (fields, expected) in cases {
        let source = format!("declaration: D, state: S, {fields}, close: stop, runtime: none");
        let error = super::expand(source.parse().unwrap()).err().unwrap().to_string();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("invalid input"), "{error}");
    }
}

/// 私有目录投影（catalog projection）验证未来撤回策略，不更改生产状态。
#[test]
fn compatibility_yank_is_rejected_and_excluded_from_aggregate() {
    use pi_ffi_abi::management::*;
    let catalog = INTERFACE_VERSIONS.iter().map(|entry| InterfaceVersion {
        interface: entry.interface, major: entry.major, symbol: entry.symbol, feature: entry.feature,
        latest: entry.latest, status: if entry.feature == "init_v1" { InterfaceVersionStatus::Yanked } else { entry.status },
        previous: entry.previous, change: entry.change,
    }).collect::<Vec<_>>();
    let names = [syn::parse_str("metadata_v1").unwrap(), syn::parse_str("init_v2").unwrap(), syn::parse_str("all_legacy").unwrap(), syn::parse_str("close_v1").unwrap()];
    let selected = super::select_versions(Some(&names), &catalog).unwrap();
    assert_eq!(selected.len(), 3);
    assert!(selected.iter().all(|entry| entry.feature != "init_v1"));
    let rejected = [syn::parse_str("init_v1").unwrap()];
    assert!(super::select_versions(Some(&rejected), &catalog).err().unwrap().to_string().contains("yanked"));
}
