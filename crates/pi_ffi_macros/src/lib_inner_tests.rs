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
    } = parsed;
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
