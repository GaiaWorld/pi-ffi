# pi_ffi

[接口版本、逐版接入和完整历史](#interface-versions-and-compatibility)。

## 用途与支持环境（Scope and support）

`pi_ffi` 是原生 Rust 插件系统（plugin system）的宿主（host）管理和插件接入库。它查找、选版、加载、初始化、维持和关闭合规插件动态库（plugin dynamic library），安全交接实际输入并释放本库取得的系统引用。插件可使用静态或动态服务（static or dynamic service），业务通信以及运行时的运行和维护由相应外部组件负责。

首批目标为 Linux x86_64／GNU 和 Windows x86_64／MSVC。库采用 Rust 2021 版次（edition）。当前已验证的编译器为 Rust `1.99.0-nightly`（`eff8269f7`），最低支持 Rust 版本（MSRV）尚未确定。设计骨架已在 Linux 构建；实际插件运行、关闭和 Windows 使用须以完成原生验证的发布为准。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 插件包（plugin package） | 宿主选定的可信动态库、配套依赖及构建记录。 |
| 配套构建（matching build） | 两侧编译与实际装入的 Rust 库、工具链、Rust 类型和分配器（allocator）满足本轮对象交接的条件。 |
| 稳定身份和版本（identity and version） | 插件 UUID 跨版本沿用；完整语义化版本（SemVer）区分本进程中的版本实例。 |
| 初始化轮次（initialization round） | 宿主准备输入后得到的原轮标识及管理句柄（handle）。初始化、重试和关闭沿同一轮。 |
| 系统引用（system reference） | 本库从操作系统加载器（OS loader）取得并归还的实际引用。其它使用者的引用可能使动态库继续映射。 |

## Cargo 功能开关（features）

`pi_ffi` 默认启用 `init_v2`、`metadata_v1`、`close_v1`，直接依赖即可使用这组最新版。额外启用 `init_v1` 增加旧三输入初始化（initialization）的能力；`all_legacy` 汇总三类管理接口（management interfaces）中全部仍可用的非最新版，目前只有 `init_v1`，与默认项或逐版项取并集。完整选择和组合见本页[接口版本与兼容声明](#interface-versions-and-compatibility)。Linux 和 Windows 随编译目标分配平台依赖。运行时可选和静态服务选择属于初始化及部署配置，依实际终端、插件和配套框架使用。

## 宿主从可信插件包装入（Host loading）

宿主准备 `HostBuild`、可信 `PluginPackage` 和 `PluginRequest`。包声明及本轮输入应来自实际部署。先用请求中的 UUID 和 `VersionReq` 判断版本，再使用标准初始化。版本范围选满足要求的最高优先级；若实际文件不同但优先级并列，显式选包。完整版本包含构建元信息（build metadata），而语义化版本优先级按 SemVer 规则判断。

| 公开类型或入口 | 使用方法 |
| --- | --- |
| `Target`、`HostBuild`、`BuildManifest` | 前者确定 Linux／Windows 目标。宿主与插件记录实际 Rust 构建摘要及分配器。插件清单记录稳定 UUID、完整版本、目标、主文件、依赖及静态声明归属。 |
| `BuildFile`、`BuildDependency`、`DependencyUse` | 文件记录受信根里的路径及 SHA-256。依赖给出实际装入名、相应文件和角色。`Plugin`、`SharedRust`、`RuntimeInput` 分别用于插件依赖、共享 Rust 配套和运行时输入配套。 |
| `StaticDeclarations` | `PrivateInventory` 为插件自有收集表，`ExplicitList` 为编译期明确清单且构造器不向共享表提交，`Empty` 为确无静态条目。 |
| `PluginPackage::from_verified_build` | 不安全入口。宿主已确认配套构建、分配方式、代码可信及部署期间的文件和装入条件，才可从实际清单构建。 |
| `PluginRequest`、`Version`、`VersionReq` | 用 UUID 和版本范围（version requirement）匹配可信候选。 |
| `Manager::process`、`find`、`load` | 取得本进程的一个管理入口，从调用者给定的包选版，并核对后装入明确插件包。读文件和系统装入是不同步骤；装入过程可能执行构造器（constructor）。 |
| `Plugin`、`Metadata`、`Plugin::metadata` | 管理实际模块。返回宿主拥有的只读 UUID／完整版本副本，初始化期间、关闭期间和完成关闭后可查。读取元信息不启动业务。 |

### 准备输入前的共享适配库核对（Shared adapter verification）

新增的 `pi_ffi_adapters::library_address()` 提供库内私有只读静态对象（static object）的地址，供加载器（loader）核实际归属。核对使用返回的数据地址；原核准文件身份和映射（mapping）身份仍须匹配，另一静态副本或另一动态库（dynamic library）不因此获得接纳资格。查询不会发布或消费初始化（initialization）输入，也不新增C管理接口（C API）或接口（API）版本。

宿主（host）、插件（plugin）及共享适配库（shared adapter library）须从包含新查询的同次配套源码重建并更新可信制品记录；不承诺任意同版本号的历史发布都具有该能力。普通调用方仍使用原准备入口，无需自行调用查询。Windows原生验证仍有缺口，不能用WSL结果代替。

## 每轮初始化、关闭与释放（Initialization and cleanup）

以下为默认初始化 v2 接法；v1 的无 D 专用方法见版本专章。宿主以经配套核实的真实 P2P 终端建立 `TerminalInput<T>`。只有在原生终端类型、实例、Rust 依赖和对应代码能保持到最后一次使用及释放时，调用 `unsafe { TerminalInput::new(terminal_arc) }`。宿主交出一份 `Config` 和初始化数据（initialization data）D，调用 `prepare_without_runtime(terminal, config, data)` 省略运行时，或调用 `prepare(terminal, config, runtime, data)` 传 `Option<Arc<R>>`。可选实例如有值须满足 `IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static` 的实际 Rust 合同。

D 只要求 `Send + Sync + 'static`，不要求 `Clone`。双方约定同一实际类型，插件通过
`Inputs<T,R,D>::data` 按值取得原值。没有额外内容时明确传 `()`。若双方选择
`D = Arc<SharedState>`，宿主可在传入前按该具体类型自己的能力克隆 `Arc`，与插件共享
同一对象。分别静态链接（static link）相同库不自动共享静态变量。
登记类型与插件预期不同会在业务初始化前返回 `Conflict`，不消费 D；配套构建前提仍须成立。

所有实际 D 的定义、目标、工具链、特性、依赖、分配器（allocator）及间接析构／回调
必须符合包的配套构建（matching build）保证；相关代码持续有效至最后一次使用与清理。
类型身份相等和 `Send + Sync + 'static` 不能单独证明任意 Rust 产物布局兼容。

```no_run
use pi_ffi::{
    config::Config,
    host::{Initialization, Manager, Plugin, PluginPackage, PluginRequest, TerminalInput},
};
use pi_result::Result;

fn prepare_round<T: Send + Sync + 'static, D: Send + Sync + 'static>(
    manager: &Manager,
    package: &PluginPackage,
    request: &PluginRequest,
    terminal: TerminalInput<T>,
    config: Config,
    data: D,
) -> Result<(Plugin, Initialization)> {
    // terminal 来自实际宿主的配套构建和原生 P2P 终端。
    let plugin = manager.load(package, request)?;
    let round = plugin.prepare_without_runtime(terminal, config, data)?;
    // 持有原轮句柄，后面的初始化即使失败也能继续关闭这个轮次。
    Ok((plugin, round))
}
```

- `Initialization::id` 读取既有轮次标识。`initialize` 发起一次实际初始化，同轮重试共享确认结果，竞争返回 `Busy`。
- `Plugin::current_initialization` 可查询本模块仍需处理的原轮，包括初始化失败后应清理的轮次。同一 UUID 与完整版本在该进程最多有一个实际初始化实例。准备或清理未完成时，原轮仍可能保留这一占用。
- `Initialization::close` 结束原轮。若有必要工作未退出，处理真实 `pi_result`，保留原轮及其进度。也可在尚未调用初始化时取消准备的轮次。
- `Manager::maintain(budget)` 有界推进已登记的归还。`Plugin::unload` 显式归还本库的实际系统引用，须等原轮、受保护的调用以及相关引用退出。报告系统释放结果不明时沿保存的实际状态处理；宿主仍可读取已有元信息副本。

## 封装插件的默认入口（Plugin wrapping）

`plugin::Declaration` 提供跨版本稳定 UUID 和完整版本。`Exports<S>::new` 建立这一实际模块的一份标准接入，`S: Send + 'static` 是插件自有的本轮状态。`Inputs<T, R = (), D = ()>` 提供原生终端、配置、可选运行时和按值的数据；T、R 的位置保持，D 在末尾。

| 入口 | 给包装者的合同 |
| --- | --- |
| `Exports::metadata` | 为受保护调用写出编译期元信息。调用者提供有效的独占输出槽。 |
| `Exports::initialize` | 按实际类型接收 `Inputs<T,R,D>`，初始化函数用 `&mut Option<S>` 保存本轮效果及清理所需数据。 |
| `Exports::initialize_without_runtime` | 对应运行时缺省形态，接收 `Inputs<T,(),D>`，终端、配置和数据必传。 |
| `Exports::close` | 按原轮标识调用 Rust 关闭函数，清理插件本轮服务和工作。未完成时以 `pi_result` 报告，保留实际进度。 |
| `pi_ffi::abi` | 标准 C ABI 名称、布局、函数指针及初始化进度，分别用于所选版本的准确导出。 |
| `pi_ffi::{config,data,runtime,service}` | 重导出 `pi_ffi_adapters` 的原生输入及静态声明收集入口，按其入口文档使用。 |

### 手工接入（Manual wrapping）

外部包装库使用 `cdylib`，依赖与宿主配套的 `pi_ffi`、`pi_result` 及 `pi_p2p_service`。示例省略运行时，保留终端、配置和共享计数。三个 C 包装共用一个 `Exports`；真实插件在产生资源或登记前建立本轮清理状态。

```ignore
use pi_ffi::{
    abi::{
        CloseFnV1, InitIdV1, InitReplyV1, InitializeFnV2, MetadataFnV1, MetadataV1, ObjectRefV1,
        OutcomeV1,
    },
    plugin::{Declaration, Exports, Inputs},
};
use pi_p2p_service::P2pServiceTerminal;
use pi_result::Result;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

type PluginInputs = Inputs<P2pServiceTerminal, (), Arc<AtomicUsize>>;

const DECLARATION: Declaration = Declaration {
    uuid: [
        0x29, 0x57, 0x00, 0x98, 0xaa, 0x48, 0x46, 0x8c,
        0xb5, 0x12, 0x63, 0xc2, 0x6b, 0x24, 0xa9, 0x31,
    ],
    version: env!("CARGO_PKG_VERSION"),
};

static EXPORTS: Exports<PluginInputs> = Exports::new(DECLARATION);

fn initialize(inputs: PluginInputs, state: &mut Option<PluginInputs>) -> Result<()> {
    inputs.data.fetch_add(1, Ordering::SeqCst);
    *state = Some(inputs);
    Ok(())
}

fn close(state: &mut Option<PluginInputs>) -> Result<()> {
    drop(state.take());
    Ok(())
}

// 遵约宿主保活模块并提供独占的输出槽。
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_metadata_v1(out: *mut MetadataV1) -> OutcomeV1 {
    unsafe { EXPORTS.metadata(out) }
}

// 遵约宿主保活同一轮的输入至调用返回。
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_initialize_v2(
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    data: *const ObjectRefV1,
) -> InitReplyV1 {
    unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, initialize) }
}

// 遵约宿主保活模块，并按原轮次发出关闭请求。
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_close_v1(id: InitIdV1) -> OutcomeV1 {
    unsafe { EXPORTS.close(id, close) }
}

const _: MetadataFnV1 = pi_ffi_metadata_v1;
const _: InitializeFnV2 = pi_ffi_initialize_v2;
const _: CloseFnV1 = pi_ffi_close_v1;
```

同一个插件制品可改用 `pi_ffi_macros::export_plugin!`，用相同声明、初始化和关闭函数生成三个 C 导出及类型检查。省略 versions 时使用默认最新组，显式声明可同时导出多版。

### 所有权（ownership）与迁移（migration）

下面的 D 没有实现 `Clone`，仍可直接接收。插件自行决定保存和业务清理方式。

```no_run
use pi_ffi::plugin::Inputs;
use pi_result::Result;

struct AppData { bytes: Vec<u8> }

fn initialize<T>(inputs: Inputs<T, (), AppData>, state: &mut Option<AppData>) -> Result<()> {
    assert!(!inputs.data.bytes.is_empty());
    *state = Some(inputs.data);
    Ok(())
}
```

从旧三输入版本迁移时，宿主准备方法末尾补数据（无内容补 `()`），手工导出改为
`pi_ffi_initialize_v2` 并接收第四指针，类型检查改用 `InitializeFnV2`；结构体字面量
补 `data`。使用宏时更新配套 `pi_ffi_macros`，保持既有声明字段，从初始化函数推断 D。
旧 v1 保留准确三参数形状；启用 init_v1 后使用专用 prepare_v1 系列。默认只接入 v2，指定版本缺席在准备前拒绝，绝不回退。

准备失败不保证把 D 退回调用者。一次交付后，同轮重试只读取已有结果，不再次取值。
初始化部分失败仍使用原轮继续关闭。本库依据三类标准接口的实际返回及自身持有处理，
不把插件内部服务清单或状态作为初始化成功判据，也不关闭传入的宿主终端或运行时。

### 使用声明、配置和关闭（Combining capabilities）

插件（plugin）可在自己的 Rust 代码中使用原生接口（native APIs），或选择 `service::register_static` 收集一次静态声明（static declarations）。空集合（empty collection）直接成功；收集时分配失败（allocation failure）在闭包（closure）前返回；非空集合（nonempty collection）只调用一次所提供的闭包（closure）并原样返回其实际结果（actual result）。本函数不保证整批操作的原子性（atomicity）、业务就绪（business readiness）或静态注销（static unregistration）。具体服务（services）能力以配套版本的原生接口（native APIs）为准。

服务（services）的全部实现（implementation）、注册（registration）、管理（management）及注销（unregistration）由插件（plugin）内部负责，对宿主管理流程（host management flow）透明。静态服务（static services）在当前 P2P 主机网络（P2P host network）中不注销（unregistration），动态服务（dynamic services）允许运行期（runtime）注销（unregistration）。标准初始化（initialization）及关闭（close）的实际返回由插件（plugin）决定，本库严格按该返回和自身持有处理，不检查内部服务（services）清单或状态（states）。持有原生终端（terminal）派生的句柄（handles）、引用或回调（callbacks）时，仍保留对应 `Terminal` 至满足这些对象的存续条件。

本轮 `Config` 可以按其原生接口直接使用。旧业务依赖其它配置版本时，按业务的原构造和优先级获得旧版真实值。运行时能力的变更在新的合法初始化轮次中选择；插件拥有本轮派生任务与引用的清理责任。

### 恐慌和错误（Panic and errors）

宿主及插件在各自承诺转换恐慌（panic）的 Rust C 边界捕获可展开恐慌。最终根工作区使用的开发及发布配置默认为 `panic = "unwind"`（目标支持展开且无覆盖时）。可在最终工作区显式声明如下。

```toml
[profile.dev]
panic = "unwind"

[profile.release]
panic = "unwind"
```

核对自己最终使用的配置、环境、实际工具链及配套 Rust 产物。Cargo 的普通源码依赖使用该构建的有效策略，依赖库自己的根配置不替最终构建选择。实际预编译或原生依赖的链接和异常合同须另核。可预期失败使用 `pi_result::Result`。栈展开的捕获不会回滚业务效果，进度仍由本轮记录确认。 [Cargo 配置说明](https://doc.rust-lang.org/cargo/reference/profiles.html#panic) · [Rust FFI 展开规则](https://doc.rust-lang.org/reference/panic.html#unwinding-across-ffi-boundaries)。

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
