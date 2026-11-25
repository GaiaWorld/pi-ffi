# pi_ffi_macros

## 用途与支持环境（Scope and support）

`pi_ffi_macros` 为 Rust 插件动态库（plugin dynamic library）的包装者提供可选过程宏（procedural macro）`export_plugin!`。它根据已有 Rust 初始化、关闭函数及插件声明生成标准管理导出，适用于希望减少重复接线的外部包装库。过程宏在编译器（compiler）内工作，生成代码使用包装库实际依赖的 `pi_ffi`。包装者也可直接使用 `pi_ffi::plugin::Exports` 手工接入。

插件的首批目标平台是 Linux x86_64／GNU 和 Windows x86_64／MSVC。库采用 Rust 2021 版次（edition），已验证的编译器是 Rust `1.99.0-nightly`（`eff8269f7`）。Linux 的宏骨架构建和手工／宏展开模板的导出形状已检查，宏的实际展开及完整行为在实现完成后验证。最低支持 Rust 版本（MSRV）尚未确定。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 包装库（wrapper crate） | 下游 Rust 库，含实际插件逻辑以及编译期选定的标准接入。以 `cdylib` 产出动态库。 |
| 声明（declaration） | `pi_ffi::plugin::Declaration` 常量，提供跨版本稳定 UUID 和完整语义化版本（SemVer）。 |
| 初始化状态（initialization state） | 插件每轮拥有的 Rust 状态，用于记录实际效果和收尾进度。 |
| 标准导出（standard export） | pi-ffi 定义的只读元信息、初始化和关闭三个同步 C 入口。 |

## Cargo 功能开关（features）

库清单目前无公开功能开关（features），直接依赖默认配置。一个插件制品选择一次宏调用或一套手工入口。这项唯一性由最终符号检查，适用于同一制品里混写和重复宏调用。

## 生成标准入口（Export generation）

`export_plugin!` 的必填字段是 `declaration`、`state`、`initialize`、`close`、`runtime`。可选字段 `api` 默认使用 `::pi_ffi`。字段可以换序并允许尾逗号。初始化、关闭函数返回 `pi_result::Result<()>`，插件状态须满足 `Send + 'static`。

| 字段 | 类型及用途 |
| --- | --- |
| `declaration` | 指向 `Declaration` 常量的 Rust 路径。 |
| `state` | 插件自己的状态类型。使用 `&mut Option<State>` 保留本轮真实进度。 |
| `initialize` | 指向原生初始化函数，其参数为对应的 `Inputs<T, R, D>` 与本轮状态；从该函数推断 D，无需新增宏字段。 |
| `close` | 指向清理函数，其参数为同一类型的本轮状态。 |
| `runtime` | `none` 使用仅接受缺省运行时的接法；`optional(MyRuntime)` 允许有无该类型的实际运行时。 |
| `api` | 显式指定 Cargo 依赖重命名后在 Rust 中使用的路径。 |

### 省略可选运行时（No runtime input）

在外部包装库里，终端、配置和初始化数据（initialization data）D 必传。以下示例选择
`D = Arc<AtomicUsize>`，与宿主共享同一计数。宏创建 `Exports` 及三个匹配 C 签名的函数。

```ignore
use pi_ffi::plugin::{Declaration, Inputs};
use pi_p2p_service::P2pServiceTerminal;
use pi_result::Result;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

type State = Inputs<P2pServiceTerminal, (), Arc<AtomicUsize>>;

const PLUGIN: Declaration = Declaration {
    // 选择一次真实身份，后续发布版本沿用同一个 UUID。
    uuid: [0x29, 0x57, 0x00, 0x98, 0xaa, 0x48, 0x46, 0x8c,
           0xb5, 0x12, 0x63, 0xc2, 0x6b, 0x24, 0xa9, 0x31],
    version: env!("CARGO_PKG_VERSION"),
};

fn initialize(inputs: State, state: &mut Option<State>) -> Result<()> {
    // D 是宿主按值传来的同一 Arc；真实插件在产生效果前建立清理记录。
    inputs.data.fetch_add(1, Ordering::SeqCst);
    *state = Some(inputs);
    Ok(())
}

fn close(state: &mut Option<State>) -> Result<()> {
    // 这里没有登记或后台工作。本例在本轮关闭时归还持有。
    drop(state.take());
    Ok(())
}

pi_ffi_macros::export_plugin! {
    declaration: PLUGIN,
    state: State,
    initialize: initialize,
    close: close,
    runtime: none,
}
```

带运行时的插件用 `runtime: optional(MyRuntime)` 并声明 `Inputs<P2pServiceTerminal, MyRuntime, AgreedData>`。Rust 编译器检查实际运行时类型（runtime type）的标准约束，同时从回调推断 D。若把 `pi_ffi` 在 Cargo 重命名为 `my_ffi`，显式写 `api: ::my_ffi`。

D 只要求 `Send + Sync + 'static`，可为不实现 `Clone` 的自有类型；没有附加内容时用
`Inputs<P2pServiceTerminal>`（D 默认 `()`），宿主仍显式传 `()`。宏不复制 D，也不
强制 `Arc`。具体定义、配套 Rust 构建、分配器（allocator）及析构／回调代码存续由双方核实。

### 签名、诊断和安全前提（Signatures, diagnostics, and safety）

生成的符号是 `pi_ffi_metadata_v1`、`pi_ffi_initialize_v2`、`pi_ffi_close_v1`，初始化按
`InitializeFnV2` 核四指针，顺序为可选运行时、必传终端、配置、数据。旧调用迁移时
更新配套 `pi_ffi`／本宏库，不新增 `data` 字段；新加载流程不回退旧 v1。只读元信息取自声明。
初始化及关闭使用同一个本轮 `Exports` 状态。未知、重复或缺失字段在编译时报告位置；
Rust 类型与链接错误由编译器及链接器（linker）报告。

宿主和插件须用同一配套构建（matching build）建立实际可用的 Rust 对象交接。初始化、静态服务登记、业务线程及关闭进度由插件 Rust 代码负责。真实的错误和未完成清理使用 `pi_result`，清理需要继续时保留原轮状态。C 入口的栈展开（stack unwinding）合同仍由实际 Rust 边界实现。宏调用示例连同 Windows／Linux 的实际导出及手工等价性，使用目标平台的真实制品验证。
