# pi_ffi_macros

[接口版本、逐版接入和完整历史](#interface-versions-and-compatibility)。

## 用途与支持环境（Scope and support）

`pi_ffi_macros` 为 Rust 插件动态库（plugin dynamic library）的包装者提供可选过程宏（procedural macro）`export_plugin!`。它根据已有 Rust 初始化、关闭函数及插件声明生成标准管理导出，适用于希望减少重复接线的外部包装库。过程宏在编译器（compiler）内工作，生成代码使用包装库实际依赖的 `pi_ffi`。包装者也可直接使用 `pi_ffi::plugin::Exports` 手工接入。

插件的首批目标平台是 Linux x86_64／GNU 和 Windows x86_64／MSVC。库采用 Rust 2021 版次（edition），已验证的编译器是 Rust `1.99.0-nightly`（`eff8269f7`）。Linux 的宏骨架构建和手工／宏展开模板的导出形状已检查，宏的实际展开及完整行为在实现完成后验证。最低支持 Rust 版本（MSRV）尚未确定。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 包装库（wrapper crate） | 下游 Rust 库，含实际插件逻辑以及编译期选定的标准接入。以 `cdylib` 产出动态库。 |
| 声明（declaration） | `pi_ffi::plugin::Declaration` 常量，提供跨版本稳定 UUID 和完整语义化版本（SemVer）。 |
| 初始化状态（initialization state） | 插件每轮拥有的 Rust 状态，用于记录实际效果和收尾进度。 |
| 标准导出（standard export） | pi-ffi 定义的只读元信息、初始化和关闭三类同步 C 接口。 |

## Cargo 功能开关（features）

库清单目前无公开功能开关（features），直接依赖默认配置。一个插件制品选择一次宏调用或一套手工入口。这项唯一性由最终符号检查，适用于同一制品里混写和重复宏调用。

## 生成标准入口（Export generation）

`export_plugin!` 的必填字段是 `declaration`、`state`、`initialize`、`close`、`runtime`。可选字段 `versions` 显式指定导出版本集合，省略时只生成各类最新版，即元信息（metadata）v1、初始化（initialization）v2、关闭（close）v1；可选字段 `api` 默认使用 `::pi_ffi`。`initialize` 支持单个回调（callback）函数路径或逐版本回调映射：只选一个初始化版本时可用路径，选择多个时按版本分别指定。v1 回调接收 `InputsV1<T,R>`，没有 D；v2 回调接收 `Inputs<T,R,D>`，仅从 v2 回调推断 D。各版共用插件状态和关闭函数。字段可以换序并允许尾逗号。初始化、关闭函数返回 `pi_result::Result<()>`，插件状态须满足 `Send + 'static`。

| 字段 | 类型及用途 |
| --- | --- |
| `declaration` | 指向 `Declaration` 常量的 Rust 路径。 |
| `state` | 插件自己的状态类型。使用 `&mut Option<State>` 保留本轮真实进度。 |
| `versions` | 可选的显式导出版本集合；省略时只生成各类最新版，可同时声明兼容版本，重叠项去重。 |
| `initialize` | 单个回调（callback）函数路径或逐版本回调映射；单版可用路径，多版须分别指定。v1 接收 `InputsV1<T,R>` 与本轮状态，没有 D；v2 接收 `Inputs<T,R,D>` 与本轮状态，仅从 v2 回调推断 D，无需新增宏字段。 |
| `close` | 指向清理函数，其参数为同一类型的本轮状态。 |
| `runtime` | `none` 使用仅接受缺省运行时的接法；`optional(MyRuntime)` 允许有无该类型的实际运行时。 |
| `api` | 显式指定 Cargo 依赖重命名后在 Rust 中使用的路径。 |

### 省略可选运行时（No runtime input）

以下是默认初始化 v2 的外部包装库接法，终端、配置和初始化数据（initialization data）D 必传。示例选择
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

<a id="interface-versions-and-compatibility"></a>

## 接口版本与兼容声明（Interface versions and compatibility）

### 版本分别表示什么

初始化、只读元信息和关闭是三类管理接口（management interfaces），每类各自演进。初始化 v2 与元信息 v1、关闭 v1 可以构成完整管理流程。名字中的版本属于 C 接口规范，不是插件业务版本，也不是 pi_ffi 包版本。

同一版本的名称、参数、返回布局和语义保持固定。不兼容变化使用新的导出名称。启用旧版能力后仍按旧版准确签名调用，不把三参数初始化当成四参数函数。

### 最新版、全部可选旧版及相邻变化

以下列出全部曾实现的接口版本，包括本次未启用者。当前没有实际被 yank 的版本。

| 类别 | 最新版 | 可选旧版 | 准确导出名／对应特性 | 允许状态 | 输入与返回 | 相对前版的关键变化 |
| --- | --- | --- | --- | --- | --- | --- |
| 元信息（metadata） | v1 | 无 | `pi_ffi_metadata_v1`／`metadata_v1` | 可用（Available） | 输出 `MetadataV1`，返回 `OutcomeV1`；读取稳定 UUID 和完整语义化版本 | 首版，无前版 |
| 初始化（initialization） | v2 | — | `pi_ffi_initialize_v2`／`init_v2` | 可用（Available） | 可选 runtime、必传 terminal／config／data 四个受控对象指针；返回 `InitReplyV1` | 相对 v1 增加第四输入 D，按值交付、不要求 Clone；新增数据预核、唯一消费及交付前失败清理；共同返回布局保持 |
| 初始化（initialization） | — | v1 | `pi_ffi_initialize_v1`／`init_v1` | 可用（Available） | 可选 runtime、必传 terminal／config 三个受控对象指针；返回 `InitReplyV1` | 首版，没有第四项 D；不接受也不隐式忽略数据 |
| 关闭（close） | v1 | 无 | `pi_ffi_close_v1`／`close_v1` | 可用（Available） | 接收 `InitIdV1`，返回 `OutcomeV1`；旧标识不能关闭新轮 | 首版，无前版 |

“旧版”按各类别判断：metadata_v1 和 close_v1 仍为最新版，不属于当前全部旧版汇总。`InitReplyV1`、`ObjectRefV1` 是共同表示的版本名；初始化升级为 v2 不要求把它们机械改名或停用。

### 编译期特性（Cargo features）

默认只启用 `init_v2,metadata_v1,close_v1`。单独启用 `init_v1` 增加旧初始化兼容，不关闭新版。汇总特性 `all_legacy` 启用三类接口全部未被 yank 的非最新版；当前精确为 `init_v1`。

```toml
[dependencies]
# 默认：初始化 v2、元信息 v1、关闭 v1。
pi_ffi = "0.1"
pi_result = "0.2"
```

增加指定旧版：

```toml
[dependencies]
pi_ffi = { version = "0.1", features = ["init_v1"] }
pi_result = "0.2"
```

显式接纳全部旧版：

```toml
[dependencies]
pi_ffi = { version = "0.1", features = ["all_legacy"] }
pi_result = "0.2"
```

只使用完整旧规范集合：

```toml
[dependencies.pi_ffi]
version = "0.1"
default-features = false
features = ["init_v1", "metadata_v1", "close_v1"]

[dependencies]
pi_result = "0.2"
```

`all_legacy` 自己不包含最新版，不替代完整默认组：关闭默认后仅选 `all_legacy`，当前会缺 metadata／close，pi_ffi 会在编译期明确报错。ABI 独立声明库保留全部准确类型和历史目录；类型存在不表示管理库编入了对应调用能力。

| 声明 | 当前编入版本 |
| --- | --- |
| 默认 | init_v2、metadata_v1、close_v1 |
| 默认加 init_v1 | init_v1／v2、metadata_v1、close_v1 |
| 默认加 all_legacy | 同上 |
| 默认加 all_legacy 和 init_v1 | 同上，重叠项只算一次 |
| 关闭默认，逐项选 init_v1、metadata_v1、close_v1 | 只有完整旧规范集合 |
| 关闭默认，只选 all_legacy | 缺少两个类别；不能作为完整管理配置 |

特性由 Cargo 对同一依赖合并。一个依赖路径没有写 init_v1，不代表另一路径不能启用它；要严格限制构建范围，须检查最终依赖图。本套版本能力由 pi_ffi 的特性控制，宏使用处仍独立决定导出什么，不因特性合并而扩大。

### 宿主使用初始化 v2

现有准备方法保持四项输入含义：

```text
Plugin::prepare(terminal, config, runtime, data)
Plugin::prepare_without_runtime(terminal, config, data)
```

D 只要求 `Send + Sync + 'static`，不要求 Clone 或 Default。双方协商实际类型；需要共享时可约定 Arc<T>，宿主按具体类型自己的能力保留共享持有。没有额外内容时传 `()`，v2 的第四 C 指针仍必传。

```ignore
let plugin = manager.load(&package, &request)?;
let round = plugin.prepare_without_runtime(terminal, config, data)?;
// 以下使用原初始化和关闭方法。
let initialized = round.initialize();
let closed = round.close();
// 分别处理并保留两个结果；关闭未完成时继续持有 round。
// 不要只传播第一个错误而丢失另一个结果。
```

实际调用方应保留原轮次并按标准结果处理失败／在途／后续关闭，不能仅因初始化失败就卸载映像。

### 宿主使用初始化 v1

v1 使用专用方法，签名中没有 D。方法如下：

```text
Plugin::prepare_v1(terminal, config, runtime)
Plugin::prepare_v1_without_runtime(terminal, config)
```

```ignore
let plugin = manager.load(&package, &request)?;
let round = plugin.prepare_v1_without_runtime(terminal, config)?;
let initialized = round.initialize();
let closed = round.close();
// 分别处理并保留两个结果；关闭未完成时继续持有 round。
```

宿主必须编入 init_v1，实际插件也必须提供有效 v1。只有 v1 的插件不支持有 D 的 v2 准备方法；只有 v2 的插件不支持专用 v1 方法，应在调用实际初始化前明确返回不支持。

同一插件同时提供 v1／v2 时，准备方法精确选择对应版本；该轮次的重试和清理继续沿原选择。不会因某版失败再转调另一版，也不会为了旧版而丢弃 D。

### 插件显式同时提供多个版本

过程宏（procedural macro）需另行依赖 `pi_ffi_macros`。它不由 pi_ffi 重导出；pi_result 仍为独立错误库。用法为：

```toml
[dependencies]
pi_ffi = { version = "0.1", features = ["all_legacy"] }
pi_ffi_macros = "0.1"
pi_result = "0.2"
```

上表列出版本相关依赖。示例源码还使用宿主配套的 pi_p2p_service，须从同一实际依赖来源引入；不能仅凭相同包名或任选版本代替配套构建。实际配套构建必须固定完整清单及解析来源。

先明确每版真实输入与共享状态：

```ignore
use pi_ffi::plugin::{Declaration, Inputs, InputsV1};
use pi_p2p_service::P2pServiceTerminal;
use pi_result::Result;

// 本例不要求这份数据可克隆。
struct AppData(Vec<u8>);

enum State {
    Legacy(InputsV1<P2pServiceTerminal>),
    Current(Inputs<P2pServiceTerminal, (), AppData>),
}

const PLUGIN: Declaration = Declaration {
    uuid: [0x29, 0x57, 0x00, 0x98, 0xaa, 0x48, 0x46, 0x8c,
           0xb5, 0x12, 0x63, 0xc2, 0x6b, 0x24, 0xa9, 0x31],
    version: env!("CARGO_PKG_VERSION"),
};

fn start_v1(inputs: InputsV1<P2pServiceTerminal>, state: &mut Option<State>) -> Result<()> {
    // v1 不依赖宿主的 AppData；真实业务由插件自己实现。
    *state = Some(State::Legacy(inputs));
    Ok(())
}

fn start_v2(
    inputs: Inputs<P2pServiceTerminal, (), AppData>,
    state: &mut Option<State>,
) -> Result<()> {
    *state = Some(State::Current(inputs));
    Ok(())
}

fn stop(state: &mut Option<State>) -> Result<()> {
    // 本例仅归还所持输入；不关闭共享运行时或 P2P 终端。
    drop(state.take());
    Ok(())
}

pi_ffi_macros::export_plugin! {
    versions: [metadata_v1, init_v1, init_v2, close_v1],
    declaration: PLUGIN,
    state: State,
    initialize: {
        init_v1: start_v1,
        init_v2: start_v2,
    },
    close: stop,
    runtime: none,
}
```

同样的版本集合可以显式写成汇总形式：

```text
versions: [metadata_v1, init_v2, close_v1, all_legacy]
```

当前它仍要求真正提供 init_v1 和 init_v2 两个初始化接法，不能因某版未编入就静默缩小“全部”的含义。也可以只列 metadata_v1、init_v1、close_v1 提供纯旧规范插件。

当前例子产生 metadata_v1、initialize_v1、initialize_v2、close_v1 四个实际符号，属于三类管理接口。宏只生成一份 Exports<State>；各初始化版本及关闭引用同一份状态。每一有效同轮跨版本请求返回 Conflict，不重复执行另一初始化函数；同版本重复请求沿原稳定结果和 Busy 规则处理。

`runtime: optional(MyRuntime)` 沿现有含义，v1 回调接收 InputsV1<T,MyRuntime>，v2 回调接收 Inputs<T,MyRuntime,D>。R 的原生特征与配套构建要求保持；关闭不允许关闭传入的共享实例。

版本集合中的重叠项只生成一次；回调映射不能重复定义同一版本。未知版本、已 yank 版本、缺少所选版本回调、回调签名不符或目标 pi_ffi 未编入所需能力时应编译失败。不能把四指针回调强转成三指针回调。

### 准确的 C 签名、结果与错误

Rust 接口（Rust APIs）的共同约束如下，v1 不引入 D：

```text
S: Send + 'static
T: Send + Sync + 'static
R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static
D: Send + Sync + 'static
InputsV1<T,R=()> { terminal: Terminal<T>, config: Config, runtime: Option<Runtime<R>> }
Inputs<T,R=(),D=()> { terminal: Terminal<T>, config: Config, runtime: Option<Runtime<R>>, data: D }
Plugin::prepare_v1<T,R>(&self, TerminalInput<T>, Config, Option<Arc<R>>) -> Result<Initialization>
Plugin::prepare_v1_without_runtime<T>(&self, TerminalInput<T>, Config) -> Result<Initialization>
Plugin::prepare<T,R,D>(&self, TerminalInput<T>, Config, Option<Arc<R>>, D) -> Result<Initialization>
Plugin::prepare_without_runtime<T,D>(&self, TerminalInput<T>, Config, D) -> Result<Initialization>
unsafe Exports<S>::initialize_v1<T,R>(&self, runtime, terminal, config, impl FnOnce(InputsV1<T,R>, &mut Option<S>) -> Result<()>) -> InitReplyV1
unsafe Exports<S>::initialize_v1_without_runtime<T>(&self, runtime, terminal, config, impl FnOnce(InputsV1<T>, &mut Option<S>) -> Result<()>) -> InitReplyV1
unsafe Exports<S>::initialize<T,R,D>(&self, runtime, terminal, config, data, impl FnOnce(Inputs<T,R,D>, &mut Option<S>) -> Result<()>) -> InitReplyV1
unsafe Exports<S>::initialize_without_runtime<T,D>(&self, runtime, terminal, config, data, impl FnOnce(Inputs<T,(),D>, &mut Option<S>) -> Result<()>) -> InitReplyV1
```

上表接收方法的 runtime、terminal、config、data 均为 `*const ObjectRefV1`，顺序与 C 签名相同；without_runtime 方法拒绝非空 runtime。`TerminalInput::new` 的不安全前提（unsafe precondition）要求 T 为实际配套的 P2P 终端类型，不能把任意满足线程约束的类型当成真实终端。初始化方法返回后，继续用原 `Initialization::initialize/close`；状态 S 由各版回调和共同关闭回调管理。


以下是所有已实现规范的函数指针类型（function pointer types），参数顺序是合同的一部分：

```text
MetadataFnV1 = unsafe extern "C" fn(*mut MetadataV1) -> OutcomeV1
InitializeFnV1 = unsafe extern "C" fn(runtime: *const ObjectRefV1, terminal: *const ObjectRefV1, config: *const ObjectRefV1) -> InitReplyV1
InitializeFnV2 = unsafe extern "C" fn(runtime: *const ObjectRefV1, terminal: *const ObjectRefV1, config: *const ObjectRefV1, data: *const ObjectRefV1) -> InitReplyV1
CloseFnV1 = unsafe extern "C" fn(InitIdV1) -> OutcomeV1
```

元信息（metadata）输出槽必须独占可写；成功后在模块保护结束前复制 UUID 和版本字节。初始化只有 runtime 可以为空；v1 不接收 data，v2 即使 D=() 也必须传真实第四输入。输入必须来自同轮同步借用的原地址和真实来源；不能复制描述冒充借用。

初始化返回 `InitReplyV1`：`INIT_COMPLETED` 仅与成功配对，`INIT_NOT_STARTED` 表示明确未消费即失败，`INIT_NEEDS_CLOSE` 表示可能有插件效果，须沿原轮关闭。`OutcomeV1.kind == 0` 表示成功；失败保持原错误分类及完整来源诊断，无法提供来源诊断时使用非零 `diagnostic_failure_kind`，不能把缺失诊断解释成成功。

缺指定真实入口时，prepare 在预留轮次和发布输入前返回 `Unsupported`。没有任何允许初始化入口的制品在装入前返回 `NotFound`；允许但损坏的直接导出为 `Corrupted`。同轮换版本或完整租约为 `Conflict`；同版在途为 `Busy`，稳定结果重放且不再次消费。当前轮已关闭后同版初始化为 `InvalidState`，历史标识不能影响新轮。业务拒绝、可捕获恐慌（panic）及清理失败均按实际进度保留责任，不能换版重试。

关闭未完成返回 `Busy` 时保留原轮继续处理；清理失败不会由空状态覆盖成成功。新一轮必须等原轮按合同退出。v2 的输入失败不承诺退回 D，保护归还报错时也可能已取值；交付前由接收侧负责受保护清理，交付后由插件按其业务合同处理。关闭不关闭宿主的运行时（runtime）或 P2P 终端（P2P service terminal）。

### 手工包装与准确签名

手工接入和宏使用同一标准。一份 Exports<S> 对应实际模块共享状态；v1 C 包装调用三输入接收方法，v2 包装调用四输入方法。分别用 MetadataFnV1、InitializeFnV1、InitializeFnV2、CloseFnV1 核对类型。需要两个初始化版本时导出两个准确名字，共用原关闭责任。


沿用前例的 PLUGIN、State、start_v1、start_v2、stop；手工包装与宏声明二选一，不能重复定义同名导出：

```ignore
use pi_ffi::{abi::*, plugin::Exports};
static EXPORTS: Exports<State> = Exports::new(PLUGIN);

#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_metadata_v1(out: *mut MetadataV1) -> OutcomeV1 {
    unsafe { EXPORTS.metadata(out) }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_initialize_v1(runtime: *const ObjectRefV1, terminal: *const ObjectRefV1, config: *const ObjectRefV1) -> InitReplyV1 {
    unsafe { EXPORTS.initialize_v1_without_runtime(runtime, terminal, config, start_v1) }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_initialize_v2(runtime: *const ObjectRefV1, terminal: *const ObjectRefV1, config: *const ObjectRefV1, data: *const ObjectRefV1) -> InitReplyV1 {
    unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, start_v2) }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_close_v1(id: InitIdV1) -> OutcomeV1 {
    unsafe { EXPORTS.close(id, stop) }
}
const _: MetadataFnV1 = pi_ffi_metadata_v1;
const _: InitializeFnV1 = pi_ffi_initialize_v1;
const _: InitializeFnV2 = pi_ffi_initialize_v2;
const _: CloseFnV1 = pi_ffi_close_v1;
```

签名和导出存在性不单独证明真实业务实现或 Rust 对象的构建兼容。旧规范支持仍要求可信制品、实际依赖、相同原生类型／共享制品、分配器及代码存续核验；任意历史二进制并不自动获得兼容资格。

### yank 与完整历史

某接口版本只有在明确决定后才标记 yank。标记后不再允许通过逐版本特性、汇总项或宏声明选择和使用它；旧 API 和解释仍保留在本章及相关注释，标明禁止状态、原签名、停用原因、适用发布和迁移条件。历史代码说明不等于恢复可编译的入口。

汇总项始终排除被 yank 的版本。实际撤回发布移除对应活动特性、接收／调用方法和活跃函数别名；显式 Cargo 请求该名字会失败，装入前拒绝含已知被禁标准导出的制品，即使它还提供其它可用版本。完整历史、符号名和仍共用的数据表示保留；全部有效特性构建不包含已撤回项。不能保留“启用必报错”的名字，又承诺 --all-features 始终成功。

这项政策按所使用的库发布固定。它不执行 cargo yank、不访问远程控制服务，也不会追溯修改已经编译运行的旧宿主。初始化某版的 yank 不会机械影响其它类别的相同版本号或仍共用的 V1 数据表示。

### 升级和组合注意事项

- 更新版本目录时，同步当前最新版、所有可选旧版、yank 状态、宏别名展开、注释、README 和对应验证，不在运行中重新解释“最新”。
- 逐接口版本能分别启用，不代表所有未来跨版本三元组天然合法；本库只调用明确支持的完整组合。当前为 metadata_v1／init_v1／close_v1 和 metadata_v1／init_v2／close_v1。
- 在途请求、重复结果、失败清理和关闭必须沿首次确定的版本及原轮次，不能把错误、Busy 或 panic 当成改版重试信号。
- v1 旧接法与 v2 有 D 的接法分别承担准确输入及清理义务；插件如何保存、使用和处理业务状态对宿主管理透明。
- 生成文档不应要求开启旧版才能看见历史，也不使用 cfg(doc) 恢复被禁止调用的 API。需要只看某一版时按章节阅读，完整历史继续存在。
- 实际 yank、移除旧特性、改变默认最新组都需要明确发布及迁移决定，不能描述为所有旧源码和二进制都保持不变。
