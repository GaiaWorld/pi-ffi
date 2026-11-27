#![doc(html_no_source)]
#![doc = include_str!("../README.md")]

#[path = "lib_inner.rs"]
mod inner;

use pi_result::prelude::ThisError;
use proc_macro::TokenStream;
use syn::{
    parse::{Parse, ParseStream},
    Path, Type,
};

struct PluginDeclaration {
    api: Path,
    declaration: Path,
    state: Type,
    versions: Option<Vec<syn::Ident>>,
    initialize: InitializationCallbacks,
    close: Path,
    runtime: RuntimeParameter,
}

// 版本选择（version selection）保持原始位置，便于重复／遗漏诊断。
enum InitializationCallbacks {
    Single(Path),
    Versions(Vec<(syn::Ident, Path)>),
}

enum RuntimeParameter {
    None,
    Optional(Type),
}

#[derive(Debug, ThisError)]
#[error("{message}")]
struct InvalidExport {
    message: String,
}

/// 用声明和回调（callback）生成显式版本的标准导出及准确 Rust 签名检查：
/// 省略 `versions` 时为元信息（metadata）v1、初始化（initialization）v2、关闭（close）v1。
///
/// 必填项为 `declaration`、`state`、`initialize`、`close` 和 `runtime`。
/// `declaration` 指向 `pi_ffi::plugin::Declaration` 常量。`state` 是插件
/// 持有的 `Send + 'static` 状态。单版 `initialize` 可指向 Rust 函数，多版使用
/// `{ init_v1: start_v1, init_v2: start_v2 }` 映射；`close` 指向共同关闭函数。
/// 回调按对应版本接收原生输入和本轮状态，以 `pi_result::Result<()>` 报告实际结果。
///
/// `runtime: none` 只接收缺省运行时（runtime）：v1 使用
/// `Exports::initialize_v1_without_runtime`，v2 使用 `Exports::initialize_without_runtime`。
/// `runtime: optional(RuntimeType)` 允许有无运行时：v1 调用
/// `Exports::initialize_v1::<_, RuntimeType>`，v2 调用 `Exports::initialize::<_, RuntimeType, _>`。
/// v1 回调接收 `InputsV1<T,R>`，没有数据 D；v2 的初始化数据（initialization data）
/// D 从 `Inputs<T,R,D>` 推断，只要求 `Send + Sync + 'static`，
/// 不增加宏字段（macro field）或 `Clone` 要求。
/// `api` 可省略，默认用 `::pi_ffi`。依赖改名时显式提供对应 Rust 路径。
/// 省略 versions 时只导出最新组；显式集合可增加 init_v1，initialize 此时可按版本映射。
/// 全部版本共用一份 Exports；重复选择去重，缺回调或目标能力不足时编译拒绝。
/// 同名导出只能由一份声明生成，手工定义同名导出时使用手工入口。
/// 生成的边界继续遵循配套构建、可信部署以及插件自己的退出合同。
///
/// ```ignore
/// pi_ffi_macros::export_plugin! {
///     declaration: DECLARATION,
///     state: PluginState,
///     initialize: start,
///     close: stop,
///     runtime: none,
/// }
/// ```
// @trace 1
#[proc_macro]
pub fn export_plugin(input: TokenStream) -> TokenStream {
    inner::export_plugin(input)
}

impl Parse for PluginDeclaration {
    // @trace 2
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        inner::parse_declaration(input)
    }
}

/*
解析接受具名字段和可选尾逗号。缺失的必传字段指向调用位置，重复字段、
未知字段或字段值格式错误指向相关输入。读取结构以后由 Rust 核实声明、
状态、两个函数和运行时类型。项目内错误以 pi_result 构造。syn 的解析
结果按其规定签名接入，原错误位置留在编译器侧，用户诊断使用该错误报告。

输出在一个匿名常量内持有一份静态 Exports<状态类型>。按显式 versions 集合
生成准确 unsafe extern "C" 函数；省略集合时为元信息 v1、初始化 v2、关闭 v1。
使用 ABI 的完整目录展开 all_legacy，并核目标目录修订号和实际编译能力；
分别委托相应入口。对每项赋予
pi_ffi::abi 中对应的函数指针类型。标准库名默认绝对路径。保留调用方
输入的路径、类型及位置。递归比较输入标记树内的标识，按 Rust 原始标识
的真实名称比较。生成的 Rust 项名和参数名避开这些名字且各自唯一。
宏本身在编译器中工作，无文件和网络副作用。插件状态和调用只在
生成函数所接入的现有 Exports 内，业务函数不进入过程宏。
*/
