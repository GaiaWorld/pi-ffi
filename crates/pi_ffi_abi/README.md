# pi_ffi_abi

[接口版本、逐版接入和完整历史](#interface-versions-and-compatibility)。

## 用途与支持环境（Scope and support）

`pi_ffi_abi` 定义 Rust 宿主（host）与合规插件动态库（plugin dynamic library）共用的 C 应用二进制接口（C ABI）。它提供固定布局、管理导出、传输身份和函数表（function table）声明，供两侧建立同一协议。库本身为 `no_std`。面向原生插件系统的应用通常从 `pi_ffi` 的 Rust 入口开始。

本项目首批目标平台是 Linux x86_64／GNU 和 Windows x86_64／MSVC。当前已验证的编译器（compiler）是 Rust `1.99.0-nightly`（`eff8269f7`），库采用 Rust 2021 版次（edition）。Linux 构建已核对，Windows 的原生验证（native validation）随整体交付进行。最低支持 Rust 版本（MSRV）尚未确定。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 协议主版本（protocol major version） | 一组精确声明和调用语义。接口表的身份、主版本和准确字节数共同匹配。 |
| 初始化轮次（initialization round） | 以 `InitIdV1` 识别的一次实际初始化；关闭请求继续用这个标识。 |
| 持有（holding） | `LeaseV1` 指向来源已登记的一次归还责任。复制字段仍指向同一次持有。 |
| 操作标识（operation identifier） | 重放同一实际请求时复用的标识。另一次操作使用新标识。 |
| 借用视图（borrowed view） | 在真实保护内取得的接口表和上下文地址。接收方在每次使用时仍须取得对应资格。 |

## Cargo 功能开关（features）

库清单目前无公开功能开关（features）。直接依赖默认配置即可，因而无额外开关组合或禁止组合。

## 插件公开的默认入口（Management exports）

`management` 模块的 `METADATA_V1`、`INITIALIZE_V2` 和 `CLOSE_V1` 是 `&CStr` 类型的精确 C 符号（C symbol）。对应导出函数类型分别是 `MetadataFnV1`、`InitializeFnV2` 和 `CloseFnV1`。同一产物提供唯一一套导出，宿主在实际模块有效期间核符号归属，按该函数类型调用。

| 声明 | 入参与返回 |
| --- | --- |
| `MetadataV1` | 输出槽接收稳定 UUID、版本字节地址和字节长度。版本是完整 UTF-8 语义化版本（SemVer），宿主在模块保护内复制。 |
| `InitializeFnV2`、`InitReplyV1` | 依次传运行时、终端、配置和初始化数据（initialization data）的只读 `ObjectRefV1` 指针。首项可为空，其余必传，包括数据为 `()` 时。结果独立给出进度与 `OutcomeV1`。 |
| `INIT_NOT_STARTED`、`INIT_COMPLETED`、`INIT_NEEDS_CLOSE` | 依次表示明确未开始、已经完成、原轮可能有效果需关闭。成功只能与 `INIT_COMPLETED` 配对。 |
| `CloseFnV1` | 传宿主已持有的原轮 `InitIdV1`，接收标准结果。旧标识只能作用于原轮。 |

```rust
use pi_ffi_abi::management::{CLOSE_V1, INITIALIZE_V2, METADATA_V1};

// 这些名字是构建产物中的实际 C 符号。查询时使用完整名称。
assert_eq!(METADATA_V1.to_bytes(), b"pi_ffi_metadata_v1");
assert_eq!(INITIALIZE_V2.to_bytes(), b"pi_ffi_initialize_v2");
assert_eq!(CLOSE_V1.to_bytes(), b"pi_ffi_close_v1");
```

历史 `INITIALIZE_V1`／`InitializeFnV1` 保留准确的三参数声明。默认能力为 v2；启用 init_v1 后使用专用准备入口精确选择旧版，不回退（fallback）。第四项复用共同对象表示，
不改变 `ObjectRefV1`、`InitReplyV1`、元信息或关闭的布局（layout），也不在 C 参数中
直接传输任意 Rust 类型的内存布局。实际类型、配套构建及分配器前提见 `pi_ffi` 的用法。

## 身份、借用与公共操作（Identity and access）

| 类型 | 组合使用 |
| --- | --- |
| `UuidV1`、`ApiHeaderV1` | UUID 按 16 字节比较；表头包含主版本和实际表尺寸。 |
| `InitIdV1`、`ObjectKeyV1` | 模块代际及轮号限定初始化。对象完整键由来源、槽位和代际组成。 |
| `LeaseV1`、`OperationIdV1` | 持有还包含对象键和轮号。逻辑操作含发起者与序号，同操作重试必须保持全部入参。 |
| `ObjectRefV1` | 将来源上下文、`SourceApiV1` 和其有效持有一起传输。Rust 的裸地址或非空检查本身无法建立实际保护。 |
| `InterfaceRequestV1`、`InterfaceViewV1` | 请求携带准确的接口身份、主版本和尺寸。借用结果是所请求的只读接口表及其配对上下文。 |
| `SourceApiV1` | `validate` 作即时校验。`retain` 获得独立持有。`release` 归还特定持有。`query_interface` 取得指定接口的借用视图。每次真正调用仍核资格。 |

```rust
use core::mem::size_of;
use pi_ffi_abi::{HostControlApiV1, InterfaceRequestV1, HOST_CONTROL_ID, HOST_CONTROL_MAJOR};

// 只构造请求描述。来源仍会核同轮持有及表的真实归属。
let request = InterfaceRequestV1 {
    interface_id: HOST_CONTROL_ID,
    major: HOST_CONTROL_MAJOR,
    size: size_of::<HostControlApiV1>() as u32,
};
assert_eq!(request.size as usize, size_of::<HostControlApiV1>());
```

`query_interface` 只在成功时让输出槽有效。调用方在作用域内保活本次代码、来源上下文和持有，依表声明核对函数存在、权限和槽位，再读取成功输出。使用同一返回视图内配对的表和上下文。普通对象接口对应其实际来源；管理接口的表和受限上下文来自宿主管理权威。查询时使用的租约仍属于原对象来源。

## 操作结果、诊断及管理表（Results, diagnostics, and control）

- `OutcomeV1` 成功时两个分类字段为零且诊断为空。一般失败持有独立的 `DiagnosticRefV1`。建立诊断失败时，两个分类字段分别记录原操作和诊断失败，诊断为空。
- `DiagnosticInfoV1` 包含分类与 `user`、`verbose`、`debug` 三段的 UTF-8 字节长度。由 `DiagnosticApiV1.info` 读取声明，再用 `copy` 复制到宿主缓冲区，最后用 `release` 归还实际来源。三段长度与写入量须核对。已接管且匹配的独立诊断即使读取或复制失败也须在原保护内归还一次。只读取及归还已核实来源与持有的诊断。
- `CallTargetV1` 绑定真实上下文、表、完整持有与目标种类 `TARGET_OBJECT` 或 `TARGET_DIAGNOSTIC`。`CallTokenV1` 用于归还已经获得的某次调用资格。两种标识的普通字段复制没有额外副作用。
- `SourceDescriptorV1` 提供来源上下文、对象表、诊断表和控制表。登记时的宿主管理上下文和操作标识决定初始化作用域。`SourceControlApiV1` 的 `maintain`、`try_retire`、`release_context` 分别推进真实收尾、核退役和释放空的来源上下文。`MaintenanceV1` 描述本次清理的瞬时结果。
- `HostControlApiV1` 的 `register_source`、`next_operation`、`track_holding`、`request_release`、`enter_call`、`leave_call`、`retire_source` 共同承接权限与释放。管理表用 `HOST_CONTROL_ID`、`HOST_CONTROL_MAJOR` 请求，来源控制表按 `SOURCE_CONTROL_MAJOR` 核验。普通包装者通过上层 Rust 接口操作。

在适用的 C 边界内将可展开恐慌（unwinding panic）转为约定结果，各侧维持自己的实际生命周期（lifecycle）。尽量通过 `pi_ffi_core` 和 `pi_ffi` 处理受控地址、持有、版本及 `pi_result` 分类。

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
