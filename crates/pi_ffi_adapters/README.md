# pi_ffi_adapters

[接口版本、逐版接入和完整历史](#interface-versions-and-compatibility)。

## 用途与支持环境（Scope and support）

`pi_ffi_adapters` 按初始化版本交接 Rust 插件动态库（plugin dynamic library）的原生输入。v1 必传配置（Config）和 P2P 终端（P2P service terminal），运行时（runtime）可选；v2 另必传初始化数据（initialization data）D。用于实现宿主（host）和插件的标准初始化接入，普通调用方可通过 `pi_ffi` 使用这些类型。

首批目标为 Linux x86_64／GNU 和 Windows x86_64／MSVC。库采用 Rust 2021 版次（edition），已验证的编译器为 Rust `1.99.0-nightly`（`eff8269f7`）。最低支持 Rust 版本（MSRV）尚未确定。宿主与插件须配套构建（matching build），实际使用同一份 Rust 适配库（adapter crate）、相应原生依赖和分配约定。当前配置使用 `pi_config 0.7`，运行时特征（trait）来自 `pi-async-rt_v1.0` 的 `pi-async-runtime-core`。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 初始化轮次（initialization round） | 同一次标准初始化的实际输入以该轮身份绑定。重复请求沿原轮，另一次合法初始化使用新轮。 |
| 配置（Config） | 宿主交给本轮的一份原生值。插件先用输入建立本轮连接，再取走该份配置。 |
| Rust 共享持有（shared Rust ownership） | 终端及运行时的插件封装分别共享原生实例，派生的使用期按本轮保护延续。 |
| 静态服务声明（static service declaration） | 插件（plugin）编译期提供的服务（services）条目；注册（registration）、初始化（initialization）及管理（management）由插件（plugin）自行负责。 |

## Cargo 功能开关（features）

库清单目前无公开功能开关（features），直接使用默认配置，因而无额外开关组合。提供或省略运行时是每轮的输入选项。静态声明装配的选择须与外部插件及框架的实际构建一致。

## 共享适配库地址（Shared adapter library address）

`library_address() -> *const core::ffi::c_void` 返回本库私有、大小为1字节的只读静态对象（static object）地址。返回值仅用于系统模块归属（module ownership）查询，不得解引用或写入，不取得库的所有权（ownership），也不作为跨进程（process）、跨次运行或卸载（unloading）后的稳定标识。

同一实际映射（mapping）受保护期间，可在多个线程（thread）重复查询并取得相同地址；查询不分配、不加锁、不阻塞。加载器（loader）仍须将实际归属与核准文件及映射（mapping）严格比对，不能仅凭非空地址或相同文件名接纳。

该查询由加载器（loader）内部用于配套核对。实际使用须将宿主（host）、插件（plugin）及共享适配库（shared adapter library）按包含此接口（API）的同次配套源码重建；不能只替换旧部署中的一个动态库（dynamic library）。该普通Rust查询不增加第四类C管理接口（C API），全部既有版本的输入和关闭合同保持。WSL验证不能代替Windows原生验证。

## 配置与同轮原生输入（Configuration and initialization）

| 入口 | 作用与约束 |
| --- | --- |
| `config::Config`、`config::pi_config` | 本库当前配套版本的原生配置和库导出，插件可沿该版本的普通 Rust 接口操作。 |
| `PublishedConfig::publish`、`with_input` | 宿主把一份配置移入本轮，再为标准同步调用临时借出受控表示。发布须有配套构建、分配、释放及代码存续的保证。 |
| `ConfigInput::from_input`、`connection` | 插件从当前、受保护的配置输入建立本轮连接。借用连接接收其余输入。 |
| `ConfigInput::take` | 先用本轮连接通过 `Terminal::from_input` 接收终端，通过 `Runtime::from_optional_input` 接收已提供的运行时。成功后才移动取得本轮唯一的一份配置。同一轮重复取用按 `pi_result` 报告。 |

调用低层的 `ConfigInput::from_input`、`Terminal::from_input` 或 `Runtime::from_optional_input` 接收有值输入时，遵守相应 `unsafe` 前提。参数来自 `with_input` 在同一同步初始化线程借出的本次原输入。外层保护覆盖原持有、来源代码、上下文和诊断表，两侧已核实配套构建、实际对象和分配方式。闭包（closure）中处理实际返回和诊断。取得配置后，插件按原生 Rust 接口使用。如果确认已经取走，但保护归还失败，也按实际进度处理原轮及原配置的清理。

## 初始化数据（Initialization data）

`data::PublishedData::publish(connection, data)` 接管双方协商的原 D，`with_input` 在
受保护的同步调用中借出该份数据的 C 表示。`DataInput::<D>::from_input` 预核本轮
来源、原始借用和真实类型，不消费数据。`take_into(&mut output)` 将原值移到空的
`Option<D>`；目标非空或已取走返回 `InvalidState`，错类型返回 `Conflict`。

D 只要求 `Send + Sync + 'static`，不要求 `Clone`、`Default` 或序列化（serialization）。
初始化 v1 不使用本数据输入；以下数据交接属于 v2。共享由具体 D 决定，例如 `Arc<T>`；无内容传 `()`，仍有合法第四输入。
两侧必须核同一实际定义、配套构建（matching build）、分配器（allocator）和间接析构／
回调代码的存续；类型身份检查不能独立证明任意 Rust 布局兼容。

```no_run
use pi_ffi_adapters::data::DataInput;
use pi_result::Result;

fn receive<D: Send + Sync + 'static>(input: DataInput<'_, D>, output: &mut Option<D>) -> Result<()> {
    input.take_into(output)
}
```

即使 `take_into` 返回错误，输出也可能已经是 `Some(D)`，调用方须依据实际输出交付
或受保护清理，不能当作没有消费。通常由 `pi_ffi::plugin::Exports` 统一处理，不必自行
编排低层接收。全部输入预核后，首次消费前须保留实际进度，先取 D，再取配置。
交给插件以后，数据的保存、使用和业务退出属于插件内部语义。

## 使用 P2P 终端与静态声明（Terminal and static declarations）

- `service::PublishedTerminal::publish` 接受已核实的实际 `Arc<P2pServiceTerminal<C, S>>`。通过 `with_input` 在标准调用中暂时借出。
- `Terminal<T>::from_input` 使用配置建立的本轮连接，接收当前真实终端的一份持有。`Terminal::native` 在本地持有期内借用原终端，可使用其原生异步（asynchronous）接口。克隆（clone）继续共享本轮持有。
- 插件（plugin）可选择使用 `register_static`，也可在自己的 Rust 代码中使用配套原生接口（native APIs）。本函数只收集一次静态声明（static declarations）：空集合（empty collection）直接成功；非空集合（nonempty collection）只调用一次插件（plugin）提供的闭包（closure），原样返回其 `pi_result`。收集时分配失败（allocation failure）发生在闭包（closure）前。条目可来自插件（plugin）自有收集表或编译期明确清单，仍须满足实际装配的归属和存续条件。本函数不保证整批操作的原子性（atomicity）、业务就绪（business readiness）或静态注销（static unregistration）；具体管理能力以配套版本的原生接口（native APIs）为准。
- 全部服务（services）的实现（implementation）、注册（registration）、管理（management）和注销（unregistration）属于插件（plugin）内部，对宿主管理流程（host management flow）透明。静态服务（static services）在当前 P2P 主机网络（P2P host network）中不注销（unregistration），可暂停或暂时停止；动态服务（dynamic services）允许运行期（runtime）注销（unregistration）。标准初始化（initialization）和关闭（close）的实际返回由插件（plugin）决定，本库不检查内部清单或状态（states）。原生终端（terminal）派生的独立句柄（handles）、引用或回调（callbacks）仍须保留对应 `Terminal`，在归还持有前满足这些对象的存续条件。

```no_run
use pi_ffi_adapters::service::{register_static, Terminal};
use pi_result::Result;

fn register_for_round<T, D>(
    terminal: &Terminal<T>,
    plugin_declarations: impl IntoIterator<Item = D>,
    register_with_framework: impl FnOnce(&T, Vec<D>) -> Result<()>,
) -> Result<()>
where
    T: Send + Sync + 'static,
{
    // 真正接入时，T 是配套的原生 P2P 终端类型。
    // 闭包（closure）由插件（plugin）提供，其实际效果（effects）和返回值（return values）由插件（plugin）处理。
    register_static(terminal, plugin_declarations, register_with_framework)
}
```

## 使用可选运行时（Runtime input）

`runtime::PublishedRuntime::publish` 登记真实共享运行时，再以 `with_input` 借给本轮标准调用。`Runtime<R>::from_optional_input` 接收该次有效的 Rust 原生实例，缺席时返回 `Ok(None)`，有值时核线程、真实类型、管理权威及轮次。运行时由外部拥有方创建、保持可用及关闭。插件使用派生对象及任务时保留相应保护，自行收尾其工作。

`Runtime::with_runtime` 在同步 Rust 闭包中借出 `RuntimeView<'_, R>`。闭包结束后只带走正式的本地结果。正常返回 `Ok(value)`，取得访问失败返回 `Err((error, None))`。闭包已返回但保护归还失败返回 `Err((error, Some(value)))`，原值仍归调用方，按已经发生的操作处理。克隆 `Runtime<R>` 共享已接收的持有。Rust 签名要求 `IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static`；前两项分别继承 `TaskDispatch` 和 `RuntimeTime`。可用标准特征（trait）如下。

| 特征 | 可用方法及搭配 |
| --- | --- |
| `TaskDispatch` | `create`、`create_with_config`、`submit`、`spawn`、`spawn_with_config`，使用提供方任务配置及原生返回语义。 |
| `IdleDispatch` | `submit_idle`、`idle`、`idle_with_config`、`idle_local`、`idle_local_with_config`。本地方法依提供方上下文使用。 |
| `RuntimeTime` | `timeout`、`timeout_on`、`sleep`，使用提供方计时及原生关联类型。 |
| `RuntimeWait` | `wait_one_timeout`、`wait_any_timeout`、`wait_all_timeout`，保留提供方等待的结果类型。 |
| `RuntimeQuery` | `id`、`status`、`is_driving`、`last_idle_state`、`close_state`，及 `external_task_count`、`internal_task_count`、`timer_registration_count`、`idle_task_count`、`in_flight_task_count`、`waiting_for_wake_count`、`pending_cancel_task_count`。查询用于插件使用其实际运行时。 |

```no_run
use pi_ffi_adapters::runtime::{IdleDispatch, Runtime, RuntimeQuery, RuntimeWait};
use pi_result::{Error, RawResult};

fn read_driver_status<R>(runtime: &Runtime<R>) -> RawResult<bool, (Error, Option<bool>)>
where
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    // 只在当前受保护闭包里借用原实例。返回值是本地拥有的 bool。
    runtime.with_runtime(|native| native.is_driving())
}
```

需要派发任务时，遵守原运行时特征的 `Future + Send + 'static`、本地上下文及取消合同。闭包借用 `RuntimeView` 的引用不能移出。新初始化轮可传另一实例或省略输入，原轮已使用的任务继续由该轮相应拥有者处理。

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
