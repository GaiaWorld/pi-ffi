# pi-ffi

`pi-ffi` 是面向跨平台（cross-platform）、多进程（multi-process）插件系统（plugin system）的上游支持库，负责查找、识别、加载、初始化、维护、关闭和卸载符合约定的 Rust 插件动态库（plugin dynamic library），安全交接初始化入参并支持运行时（runtime）替换。本库提供插件在标准初始化时汇总并登记其静态服务（static service）的接入。

**当前仓库包含设计骨架（design skeleton）。** 四个运行期库和可选编译期宏提供接口声明。实现中的 `todo!()` 会触发恐慌（panic）。本页手工封装示例已核对 Rust 类型和 Linux 导出；宏展开与运行验证在实现完成后进行。

工作区（workspace）各库的职责如下。

| 库（crate） | 当前职责 |
| --- | --- |
| [pi_ffi_abi](crates/pi_ffi_abi/src/lib.rs) | 固定 C 表示、身份／轮次／持有、诊断和共同操作表；不依赖业务库 |
| [pi_ffi_core](crates/pi_ffi_core/src/lib.rs) | `boundary`、`control`、`diagnostic`、`object`、`source` 五模块的安全封装与来源管理声明 |
| [pi_ffi_adapters](crates/pi_ffi_adapters/src/lib.rs) | 运行时（runtime）、配置（Config）和 P2P 终端（terminal）的接入 |
| [pi_ffi](crates/pi_ffi/src/lib.rs) | 宿主（host）管理和插件的三个标准入口 |
| [pi_ffi_macros](crates/pi_ffi_macros/src/lib.rs) | 可选过程宏（procedural macro），在编译期生成标准导出 |

## 支持范围与职责

首批正式支持与原生验证（native validation）的设计目标为 Windows x86_64／MSVC 和 Linux x86_64／GNU。这里声明的是目标范围，尚未完成这两个平台的产品验证。

本轮仅接纳符合本项目标准的 Rust 插件动态库（plugin dynamic library）。本项目提供下游实现插件所需的底层支持；符合标准的外部产物由下游提供。

| 参与方 | 负责内容 |
| --- | --- |
| 宿主（host） | 选择可信且遵守约定的外部产物，提供必传的 `pi-p2p-service` 终端实例（terminal instance）及 `pi_config::Config`，可选提供运行时（runtime）。通过既有服务框架提供发现、访问和跨进程通信（inter-process communication） |
| 本库 | 管理动态库（dynamic library）的加载、版本接纳、同步管理调用（management call）、生命周期（lifecycle）及初始化入参的安全交接与释放；提供插件标准初始化中汇总并登记静态声明的 Rust 接口 |
| 插件动态库（plugin dynamic library）的实现方 | 提供标准管理入口，使用注入能力自行管理任务（task）、业务服务（business service）和资源；负责业务声明、实现、启动、动态注册、调用及退出；遵守初始化、幂等性（idempotency）、错误、恐慌（panic）及清理约定 |

进程间通信（IPC）、QUIC 以及发现、连接和访问设施均由宿主（host）与既有服务框架提供，本项目不实现这些设施。对不可信原生代码（native code）的安全隔离（security isolation）也由外部宿主（host）承担。

插件可以没有服务，也可以使用静态服务（static service）或动态服务（dynamic service）。插件在标准初始化时汇总本插件的静态声明，并交给既有服务框架登记。空集合也可使用相同入口。服务的实现和使用由插件负责，并依照传入的 P2P 服务终端（P2P service terminal）与原框架的约定。封装者通过 `pi_ffi::service::register_static` 把本插件完整的静态声明交给配用框架，在本轮保留真实登记标识并在关闭时撤回。共享终端上的框架须具备批量登记、撤回与确认引用退出的能力。

运行时（runtime）替换属于本轮最小可行产品（MVP）目标。当前设计直接以 `../pi-async-rt_v1.0` 的核心库 `pi-async-runtime-core` 为基准；宿主封装（host wrapper）按标准特征（trait）接纳线程安全共享实例，包括符合合同的阻塞运行时（blocking runtime）或运行时池（runtime pool）。本库提供共享持有与克隆（clone），不要求原实例实现 `Clone`，也不绑定具体 `ConcurrentRuntime` 类型（type）。本轮不为旧 `../pi-async-rt` 实施接入后端（backend）；已有服务框架的实际依赖（dependency）仍需分别接线和验证。

提供运行时（runtime）时，所需能力为普通任务派发（TaskDispatch）、空闲派发（IdleDispatch）、计时（RuntimeTime）、等待（RuntimeWait）及查询（RuntimeQuery）。插件普通任务（task）的异步计算（Future）按原生 Rust 派发合同使用 `Send + 'static`，不额外要求 `Sync` 或 `Unpin`。本地任务派发（LocalTaskDispatch）用于原有本地上下文（context），合法条件由运行时库（runtime library）规定。空闲提交、等待、取消请求（cancellation request）及结果处理沿用运行时库的 Rust 接口（API）。

每轮初始化固定运行时（runtime）的有无及所提供的实例。新轮或新版本可以提供另一实例，也可以省略该输入。在途异步计算（Future）仍在原实例完成或收尾。外部运行时（runtime）的实际拥有方负责创建、推进、保持可用和关闭实例。

本库经安全 Rust 封装透传外部运行时（runtime）的有无。提供实例时，宿主（host）与插件（plugin）使用同一运行时库（runtime library）及其匹配的 Rust 接口（API）。pi-ffi 负责将宿主提供的实际实例安全交接给插件。插件动态库（plugin dynamic library）自行使用该实例并在关闭时收尾自己的工作。C 接口（C API）只用于 pi-ffi 的外部函数接口（FFI）边界。

本库依据插件的标准关闭结果及相关跨库引用判断是否可卸载（unloadable）。共享运行时（runtime）可继续为其他使用者工作。若仍有在途调用（in-flight call）或引用依赖插件代码，本库保留相应映射并按约定返回 Busy 或失败。运行时查询（RuntimeQuery）供插件自己的 Rust 代码显式调用。

## 标准入口与初始化输入

宿主（host）和插件（plugin）均使用 Rust 实现。pi-ffi 定义以下三个同步管理入口的签名（signature）和语义（semantics），插件动态库（plugin dynamic library）用 Rust 实现并导出这三个 C 接口（C API）。封装者按本库的标准、规范和公开 Rust 接口（API）手工接入，或用可选过程宏（procedural macro）生成同样的三个导出。业务服务（business service）通过既有服务框架提供。

| 入口 | 已对齐的使用约定 |
| --- | --- |
| `pi_ffi_metadata_v1` 只读元信息（read-only metadata） | 提供稳定身份声明及完整语义化版本号（semantic version）；在合法加载期间的初始化前、中、后及关闭期间、关闭后都可查询，不以业务初始化为前提 |
| `pi_ffi_initialize_v1` 初始化 | 接收必传的 `pi-p2p-service` 终端实例（terminal instance）、`pi_config::Config` 及可选的外部运行时（runtime）。返回实际初始化进度以及共同结果。宿主准备输入时已获得本轮标识（initialization identifier），需要继续清理时按原标识关闭 |
| `pi_ffi_close_v1` 关闭 | 接收本次初始化的标识（initialization identifier），明确返回成功或失败；旧标识（identifier）的延迟调用不能关闭新一轮使用 |

外部函数接口（FFI）采用目标平台的 C 应用二进制接口（C ABI）和受控表示。运行时（runtime）可省略，终端（terminal）和配置（Config）必传。由标准 Rust 接入核对本轮登记并接收真实对象，`Config` 在同一轮只移交一次。`repr(C)` 本身也不会使其内部嵌套的任意 Rust 类型（type）具备稳定表示。参见 [Rust 类型布局（type layout）说明](https://doc.rust-lang.org/reference/type-layout.html#representations)。

标准入口名称区分协议主版本（protocol major version）；同一主版本内固定调用约定（calling convention）及参数、返回值的布局（layout）。不兼容变化使用新的入口版本（entry version），它与外部产物自身的业务版本（business version）分别判断。完整声明见 [三个标准 C 接口](crates/pi_ffi_abi/src/management.rs)。

### 身份、版本与反复使用

- 稳定身份不因文件改名、复制或发布不同版本而改变。同一进程（process）中，相同身份与相同完整语义化版本号（semantic version）最多存在一个已经初始化的实例（instance）。
- 完整版本包含构建元信息（build metadata）；例如 `1.2.3+build.1` 与 `1.2.3+build.2` 在本项目中用于区分不同实例版本（instance version）。版本优先级（version precedence）、范围匹配（range matching）与实例区分分别处理，不能把它们混为同一比较规则。[语义化版本规范（Semantic Versioning）](https://semver.org/spec/v2.0.0.html)规定构建元信息（build metadata）不参与版本优先级（version precedence）。
- 初始化前检查所要求的版本范围（version range），不匹配则不进行业务初始化。范围使用 `pi_ffi::host::VersionReq`。系统加载可能已经执行构造器（constructor）；初始化前检查不表示此前没有执行外部代码。
- 同一进程（process）内重复加载同一实际模块（loaded module）时，共享其本轮实际初始化；输入冲突不能靠再次初始化解决。标准入口自身必须具有语义幂等性（semantic idempotency）。
- 关闭失败时保留必要的代码映射（code mapping）和同版本占用，仅在安全前提满足时继续尚未完成的清理。超时不等于后台工作已经停止。
- 再次初始化必须等本轮清理成功，且旧业务工作与调用资格已经退出或失效。最终卸载（unloading）还须等待所有需要代码映射（code mapping）的引用退出；只读查询及其返回数据也受此约束。
- 关闭插件时按约定归还本轮对实际输入取得的跨库持有。共享运行时（runtime）由其外部拥有方继续管理。

不同版本使用不同文件，并不能消除共享依赖（shared dependency）、符号（symbol）、配置（configuration）、文件、端口等资源冲突。本库对受控能力提供守护；实现方仍须正确获取、使用和释放其私有逻辑涉及的资源。外部配套构建和部署提供制品声明，本库核对文件、直接导出、实际映像及有关依赖；业务资源由插件及框架各自管理。

释放系统加载句柄（loader handle）不保证所有平台立即完成物理卸载（physical unloading）或重置静态状态（static state）。参见 [Windows `FreeLibrary`](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-freelibrary) 与 [POSIX `dlclose`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/dlclose.html)。若反复使用确实遇到当前无法可靠解决的保留状态问题，将依据真实证据讨论限制；目前没有采用“同版本仅能使用一次”的普遍限制。

### 共享持有（shared holding）与释放责任

共同对象合同及基础 Rust 接口（API）已经冻结。已提供的初始化输入通过受控 C 表关联同一宿主管理接口（host management interface）及同一初始化轮次。插件通过 Rust 封装取得原生终端与运行时的共享持有，以及本轮原生 `Config` 的所有权。

本地克隆（Clone）共享一个拥有者；需要另一份独立持有（independent holding）时，必须显式取得。每份独立持有在对外交付前，宿主（host）就已登记剩余释放义务及所需代码保活。最后一个本地拥有者结束时，只提交这条已有记录；提交不临时分配记录，也不执行来源回调（source callback）。宿主（host）随后推进真实释放，最终失败保留在管理状态与关闭结果中。

例如，最后一个本地拥有者销毁时，另一个线程仍在执行一次已经获准的调用：释放提交可以成功，但关闭仍须返回忙碌（Busy），直到该调用退出、真实清理完成。**提交成功、持有归还和清理完成分别判断。** 不能只依据 `Drop` 已执行就释放动态库（dynamic library）的代码映射（code mapping）。

宿主可调用 `Manager::maintain` 有界推进已登记的释放。安全 Rust 也允许通过 `mem::forget` 跳过析构（Drop），因此遗忘拥有者可能延后卸载（unloading），不能强制清零来绕过它。依据为 [Rust 关于遗忘与安全的说明](https://doc.rust-lang.org/1.100.0/std/mem/fn.forget.html#safety)。

析构（Drop）的提交路径只返回有限的 `pi_result` 分类投影，不创建新的错误报告（Report）。显式 `Object::release` 在最后一个本地拥有者需要提交时，再于允许分配的返回路径形成 `pi_result`；有其它本地别名时仅结束当前别名。最终清理是否完成，仍由宿主管理状态与关闭结果说明。

### 首次取得管理接口时的错误

首次查询失败时，接收方可能还没有建立管理连接（management connection）。标准来源必须在自身登记表内创建该次查询／`retain` 的错误诊断（diagnostic）；转发其它来源的错误时，先在本地转换为 `pi_result`，再生成自己的诊断。

接入封装在原调用已经提供的代码与上下文（context）保护内，复制同一来源的诊断，再显式归还；这条归还路径不放入析构（Drop）。来源已经拥有自己的宿主连接，由它先提交该诊断预登记的释放记录，再确认归还。重复提交已有记录不增加任务，也不即时执行来源回调（source callback）。

来源、上下文（context）或轮次不匹配的返回值按损坏（Corrupted）拒绝读取，不尝试解引用未经保护的地址。已有释放责任及代码持有保留，不能把拒绝读取当作已经完成清理。标准来源的代码保护、逐次访问核验与独立持有须同时满足；只检查指针非空并不充分。

## 接入代码（Integration）

将插件包装写在自己的 Rust 库中。用 `cdylib` 产出操作系统可装入的动态库（dynamic library）。两侧使用同一套配套构建（matching build），包括工具链（toolchain）、目标、Cargo 解算结果、分配器（allocator）、Rust 共享适配库及实际依赖。宿主用完整制品清单（build manifest）描述目标、稳定 UUID、完整版本、文件摘要、预期装入的依赖和静态声明归属。

下列路径按仓库在磁盘上的实际位置调整。示例使用真实 P2P 终端类型，保留必传配置，省略运行时（runtime），静态集合为空。

```toml
[package]
name = "example-plugin"
version = "1.2.3"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
pi_ffi = { path = "../pi-ffi/crates/pi_ffi" }
pi_result = { path = "../pi_result" }
pi_p2p_service = { path = "../pi-p2p-service" }
```

### 手工封装（Manual wrapping）

```rust
use pi_ffi::{
    abi::{
        CloseFnV1, InitIdV1, InitReplyV1, InitializeFnV1, MetadataFnV1, MetadataV1, ObjectRefV1,
        OutcomeV1,
    },
    plugin::{Declaration, Exports, Inputs},
};
use pi_p2p_service::P2pServiceTerminal;
use pi_result::Result;

type PluginInputs = Inputs<P2pServiceTerminal>;

const DECLARATION: Declaration = Declaration {
    uuid: [
        0x29, 0x57, 0x00, 0x98, 0xaa, 0x48, 0x46, 0x8c,
        0xb5, 0x12, 0x63, 0xc2, 0x6b, 0x24, 0xa9, 0x31,
    ],
    version: env!("CARGO_PKG_VERSION"),
};

static EXPORTS: Exports<PluginInputs> = Exports::new(DECLARATION);

fn initialize(inputs: PluginInputs, state: &mut Option<PluginInputs>) -> Result<()> {
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
unsafe extern "C" fn pi_ffi_initialize_v1(
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
) -> InitReplyV1 {
    unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, initialize) }
}

// 遵约宿主保活模块，并按原轮次发出关闭请求。
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_close_v1(id: InitIdV1) -> OutcomeV1 {
    unsafe { EXPORTS.close(id, close) }
}

const _: MetadataFnV1 = pi_ffi_metadata_v1;
const _: InitializeFnV1 = pi_ffi_initialize_v1;
const _: CloseFnV1 = pi_ffi_close_v1;
```

保留所选 UUID 供后续版本使用。真实封装在产生资源或静态登记等效果前保留本轮可用于关闭的状态。关闭时按本轮记录停止接纳、撤回实际登记并等待相关引用按框架约定退出。清理尚未完成时通过 `pi_result` 返回 `Busy`，保留后续需要的状态。

### 可选过程宏（Procedural macro）

包装库可追加 `pi_ffi_macros = { path = "../pi-ffi/crates/pi_ffi_macros" }`。下面的调用使用上例的 `DECLARATION`、`PluginInputs`、`initialize` 和 `close`，替换 `static EXPORTS`、三个手工函数及函数指针声明。

```rust
pi_ffi_macros::export_plugin! {
    declaration: DECLARATION,
    state: PluginInputs,
    initialize: initialize,
    close: close,
    runtime: none,
}
```

有类型明确的可选运行时时用 `runtime: optional(MyRuntime)`，初始化函数接收 `Inputs<P2pServiceTerminal, MyRuntime>`。如果 Cargo 把 `pi_ffi` 改名为 `my_ffi`，加入 `api: ::my_ffi`。每份插件动态库使用一套手工入口或一次宏调用。

### 宿主调用（Host calls）

宿主使用实际构建记录调用 `PluginPackage::from_verified_build`。这个 `unsafe` 入口要求宿主已核实受信部署、配套构建与分配器，且文件和加载条件在约定的使用期间稳定。部署方有其真实构建记录后才使用下列装入和准备方法。宿主已有自己的终端及配置。创建 `TerminalInput` 时，宿主须确认双方配套构建使用同一原生终端类型、实际实例及其代码，并保活到最后使用和释放。只有此时才可调用 `unsafe { TerminalInput::new(terminal_arc) }`。后续准备方法接受这份已核实的终端输入。

```rust
use std::sync::Arc;

use pi_ffi::{
    config::Config,
    host::{HostBuild, Initialization, Manager, Plugin, PluginPackage, PluginRequest, TerminalInput},
    runtime::{IdleDispatch, RuntimeQuery, RuntimeWait},
};
use pi_p2p_service::P2pServiceTerminal;
use pi_result::Result;

pub fn prepare_example(
    build: HostBuild,
    package: &PluginPackage,
    request: &PluginRequest,
    terminal: TerminalInput<P2pServiceTerminal>,
    config: Config,
) -> Result<(Manager, Plugin, Initialization)> {
    let manager = Manager::process(build)?;
    let plugin = manager.load(package, request)?;
    let round = plugin.prepare_without_runtime(terminal, config)?;
    Ok((manager, plugin, round))
}

pub fn prepare_with_runtime<R>(
    plugin: &Plugin,
    terminal: TerminalInput<P2pServiceTerminal>,
    config: Config,
    runtime: Option<Arc<R>>,
) -> Result<Initialization>
where
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    plugin.prepare(terminal, config, runtime)
}
```

宿主从 `plugin.metadata()` 读取自己的元信息副本。保存 `round` 后调用 `round.initialize()`。成功或失败都保留原轮的句柄（handle），关闭时调用 `round.close()`。进度尚未完成时保留本轮并根据实际 `pi_result` 推进；例如在合适时机调用 `manager.maintain(budget)` 推进已登记归还，再使用原轮次继续关闭。确认关闭成功后才显式调用 `plugin.unload()`。系统释放结果不明时，按错误报告保留占用和退出证据，只读宿主已有副本。

省略运行时的插件用 `prepare_without_runtime`。另一份用 `initialize` 接受可选运行时的插件，可与 `prepare_with_runtime` 配套使用；每个已准备轮次固定原入参。

## 恐慌（panic）策略与构建设置

**Cargo 内建的开发配置（development profile）和发布配置（release profile）默认都是 `panic = "unwind"`。** 对上述两个目标平台，如果没有其它有效设置覆盖，不显式写这一项也使用栈展开（stack unwinding）。这不是对所有 Rust 目标平台的保证；不支持栈展开（stack unwinding）的目标存在例外。依据为 [Cargo 配置（profile）及默认值说明](https://doc.rust-lang.org/1.100.0/cargo/reference/profiles.html#panic)，核验基于 Rust 1.100.0 官方文档。

需要显式表达这一要求时，将以下内容放入**最终构建所在工作区（workspace）根目录**的 `Cargo.toml`；没有工作区（workspace）时放入该项目自己的根清单（manifest）。这只是把默认选择写清楚：

```toml
[profile.dev]
panic = "unwind"

[profile.release]
panic = "unwind"
```

普通源码依赖（source dependency）跟随这次构建的有效设置，不要求逐个修改上游库。依赖库（dependency crate）自己清单（manifest）中的 `[profile]` 被忽略；包级覆盖（package override）和构建覆盖（build override）也不能单独设置 `panic`。因此，本库自己的清单（manifest）无法替下游强制决定最终设置。

仍需检查以下可能改变结果的来源：

- 根清单（manifest）中实际使用的自定义配置（custom profile）及其继承关系。
- Cargo 配置文件（configuration file）与环境变量（environment variable），例如 `CARGO_PROFILE_RELEASE_PANIC`。
- `RUSTFLAGS`、`CARGO_ENCODED_RUSTFLAGS`、目标平台设置及命令行传给编译器（compiler）的参数。
- 预编译产物（precompiled artifact）、原生依赖（native dependency）及构建工具（build tool）实际采用的设置。

Cargo 根据命令执行目录及其父目录等位置读取配置（configuration），从工作区（workspace）根目录执行时不会自动遍历各成员目录中的 `.cargo/config.toml`。排查时要核对实际命令和环境；不要通过堆叠相互矛盾的参数来建立使用约定。参见 [Cargo 配置（configuration）查找与覆盖规则](https://doc.rust-lang.org/1.100.0/cargo/reference/config.html)。

### 怎样确认实际设置

在下游实际项目中，将 `your_plugin` 替换成真实包名（package name）；产物有多个库目标（library target）时也应选择实际交付的目标：

```sh
cargo build -p your_plugin --lib --release -vv
cargo rustc -p your_plugin --lib --release -- --print cfg
```

第一条用于检查实际构建命令；第二条在对应编译单元（compilation unit）的输出中应包含 `panic="unwind"`。单个编译单元（compilation unit）的结果不能证明所有独立二进制依赖（binary dependency）都满足捕获条件。

需要把设置错误尽早暴露时，可在**承担捕获保证的实现部分**使用以下编译期检查（compile-time check）。这个独立示例可保存为 `panic_mode.rs`：

```rust
#[cfg(not(panic = "unwind"))]
compile_error!("此捕获支持要求 panic=unwind");

fn main() {
    println!("panic=unwind");
}
```

```sh
rustc --edition=2021 panic_mode.rs -o panic_mode
```

在支持栈展开（stack unwinding）的目标上，默认设置允许编译；加入 `-C panic=abort` 则应编译失败。正式集成时，检查应位于负责捕获的包装实现中；纯声明或纯加载能力不因此强制采用这一策略（strategy）。

普通测试（test）、基准测试（benchmark）、构建脚本（build script）和过程宏（procedural macro）对该设置有特殊规则，不能用 `cargo test` 通过来证明发布产物（release artifact）符合要求。过程宏（procedural macro）生成并进入目标产物的 Rust 代码，仍按目标代码的实际设置处理。参见 [Cargo 恐慌（panic）设置说明](https://doc.rust-lang.org/1.100.0/cargo/reference/profiles.html#panic)。

### 上游依赖（dependency）存在冲突时

| 情形 | 处理方式 |
| --- | --- |
| 普通源码依赖（source dependency）的清单（manifest）写了 `panic = "abort"` | 该依赖自身的配置（profile）不会覆盖根设置，按本次有效配置（profile）统一编译 |
| 根配置（profile）、环境变量（environment variable）或编译参数实际选择了中止（abort） | 在最终构建处改正，并检查实际交付产物；不能仅修改本库的清单（manifest） |
| 预编译 Rust 库（precompiled Rust library）使用了不兼容策略（strategy） | 按正确设置重新编译或更换兼容产物；不能依赖外层捕获把中止（abort）变成展开（unwind） |
| 独立动态库（dynamic library）、静态原生库（native static library）或 C/C++ 组件 | 分别核对其链接（linking）、异常（exception）及释放约定；当前 Cargo 配置（profile）不会自动改写这些产物 |
| 依赖显式调用 `std::process::abort`，或发生不可恢复的故障 | 不能通过 `catch_unwind` 恢复，应按该依赖实际失败行为判断是否适合接入 |

Rust 库（Rust library）的链接（linking）兼容要求参见 [Rust 链接（linkage）说明](https://doc.rust-lang.org/1.100.0/reference/linkage.html)；显式中止的行为参见 [`std::process::abort`](https://doc.rust-lang.org/1.100.0/std/process/fn.abort.html)。

## 恐慌（panic）捕获的保证范围

只有承诺将恐慌（panic）转换为 `pi_result` 错误的 C 边界（C boundary），才要求承担该保证的一方，其最终 Rust 产物及普通 Rust 依赖图（dependency graph）支持栈展开（stack unwinding）。插件导出入口（exported entry point）与需要此保证的宿主回调（host callback）分别在自己内部捕获，不能让恐慌（panic）逃出普通 `extern "C"` 边界后，再由另一方统一捕获。

该要求不意味着构建工具（build tool）或 C/C++ 组件必须统一采用 Rust 恐慌策略（panic strategy）。独立构建、独立链接的 Rust 运行环境也不能仅凭名称相同就当作同一捕获域。参见 [`catch_unwind` 官方保证及限制](https://doc.rust-lang.org/1.100.0/std/panic/fn.catch_unwind.html)与 [Rust 外部函数接口（FFI）展开规则](https://doc.rust-lang.org/1.100.0/reference/panic.html#unwinding-across-ffi-boundaries)。

使用者需要同时理解这些边界：

- `panic = "unwind"` 允许展开，但不会自动安装错误转换；仍须在正确位置捕获并处理。
- 初始化入口的捕获只覆盖其同步调用链（synchronous call chain）；初始化返回后继续执行的异步任务（asynchronous task）需要另外明确捕获、完成与清理责任。
- `catch_unwind` 不恢复已经发生的副作用（side effect），也不自动使部分初始化或部分清理重新可用。
- 恐慌钩子（panic hook）在捕获前执行；钩子（hook）的行为及恐慌载荷（panic payload）的析构（drop）也可能导致问题。不能未经论证就对整个生命周期（lifecycle）使用 `AssertUnwindSafe`。
- 中止（abort）、硬件故障、内存破坏以及析构（drop）展开期间不可恢复的再次恐慌（panic），均不属于可恢复保证。

栈展开（stack unwinding）不会自动关闭发布优化（release optimization）；它可能影响产物大小和部分优化空间。实际性能（performance）需要按真实发布设置测量，不承诺零成本。参见 [Rust 恐慌策略（panic strategy）说明](https://doc.rust-lang.org/1.100.0/reference/panic.html#panic-strategy)。

## `pi_result` 错误处理示例

本库全部错误定义和处理使用 `pi_result`。跨外部函数接口（FFI）时采用已经冻结的宿主自有诊断数据副本（host-owned diagnostic copy）合同：来源方提取公共分类及 `user`／`verbose`／`debug` 三段 UTF-8 文本，宿主（host）复制并构造自己拥有的错误报告（error report），原报告（report）与原内存由创建方释放。来源内部可以暂存 `Pin<Box<pi_result::Error>>`，接收方不把该指针还原为本地原生错误。

共同结果 `OutcomeV1` 只允许三种表示：成功时两个分类为零且诊断为空；操作失败时携带一个独立诊断；诊断本身无法建立时携带操作分类及诊断建立失败分类，诊断为空。未知非零分类映射为 `Other` 并保留原编号；矛盾表示按 `Corrupted` 处理。固定编号是成功 `0` 及以下依次对应 `1`—`17` 的分类：`InvalidInput`、`InvalidState`、`NotFound`、`AlreadyExists`、`Conflict`、`PermissionDenied`、`Io`、`Timeout`、`Cancelled`、`Busy`、`Unavailable`、`Unsupported`、`ResourceExhausted`、`Corrupted`、`Dependency`、`Internal`、`Other`。

文本长度按 UTF-8 字节计数，不含结尾零字节；复制前检查总长度、整数转换和容量，成功写入量必须精确一致。读取或复制失败也必须交接已经接管的诊断释放责任，次级失败不能覆盖原操作分类。原始三段文本作为本地 `CopiedDiagnostic` 保留；本地报告格式会增加自己的上下文（context），不能用重新格式化代替原始文本。诊断无法建立时，不声称恢复未取得的文本或原生附件（native attachment）。

原生 `ErrorReport`、`String`、`Vec`、`Arc` 或特征对象（trait object）不能直接当作稳定 C 表示返回；`ErrorKind` 的声明顺序也不能当作协议数字编码（protocol numeric encoding）。诊断复制不等于恢复原始 Rust 错误类型（error type）或任意不透明附件（opaque attachment）。

可预期的失败直接通过 `pi_result::Result` 返回。恐慌（panic）捕获用于处理约定范围内的意外失败，不能代替正常错误处理。

以下示例使用相邻 `pi_result 0.2.0` 已有接口（API），演示正常错误与一处**固定字符串恐慌（panic）**的转换，不是完整外部函数接口（FFI）包装或正式入口实现。

在独立示例项目的 `Cargo.toml` 中声明依赖（dependency）；`path` 按实际目录调整：

```toml
[package]
name = "pi-ffi-result-example"
version = "0.1.0"
edition = "2021"

[dependencies]
pi_result = { version = "0.2", path = "../pi_result" }
```

将以下内容保存为该项目的 `src/main.rs`：

```rust
use pi_result::prelude::*;

#[derive(Debug, ThisError)]
#[error("示例输入不能为空")]
struct EmptyInput;

#[derive(Debug, ThisError)]
#[error("示例代码发生可展开恐慌（unwinding panic）")]
struct CapturedDemoPanic;

fn validate_input(value: &str) -> pi_result::Result<()> {
    if value.is_empty() {
        return Err(ErrorReport::new(EmptyInput)
            .attach(Module("readme_example"))
            .attach(Operation("validate_input"))
            .attach(Stage("check_input"))
            .change_context(ErrorKind::InvalidInput));
    }

    Ok(())
}

fn demonstrate_panic_translation() -> pi_result::Result<()> {
    std::panic::catch_unwind(|| -> () {
        panic!("受控示例：固定字符串载荷（string payload）");
    })
    .map_err(|payload| {
        // 本例载荷（payload）来自上面的固定字符串，释放不会调用用户析构器（destructor）。
        // 任意外部代码的载荷（payload）需要另行处理其析构（drop）风险。
        drop(payload);
        ErrorReport::new(CapturedDemoPanic)
            .attach(Module("readme_example"))
            .attach(Operation("demonstrate_panic_translation"))
            .change_context(ErrorKind::Internal)
    })
}

fn main() -> pi_result::Result<()> {
    validate_input("example")?;

    let invalid = validate_input("").expect_err("应返回输入错误");
    assert_eq!(invalid.current_kind(), &ErrorKind::InvalidInput);
    println!("{}", debug_message(&invalid));

    let captured = demonstrate_panic_translation().expect_err("应转换示例恐慌（panic）");
    assert_eq!(captured.current_kind(), &ErrorKind::Internal);
    println!("{}", debug_message(&captured));

    Ok(())
}
```

在该独立项目中运行 `cargo run`。默认恐慌钩子（panic hook）可能仍向标准错误（stderr）打印恐慌（panic）消息；它不表示捕获失败。示例应继续打印两份详细错误报告（error report）并正常退出。这个示例仅证明所示普通错误与受控同步恐慌（synchronous panic）的处理；它不验证异步退出（asynchronous shutdown）、任意载荷（payload）、跨库表示或卸载（unloading）安全。

## 插件实现方的服务接入与清理注意事项

插件通过既有服务框架提供异步远程过程调用（asynchronous RPC），其接口（API）和行为由插件及框架维护，对本库完全透明。标准管理导出层无需提供通用 Rust 异步计算（Future）或轮询（poll）／唤醒（wake-up）协议；内部运行时（runtime）封装仍须定义必要安全合同，不增加业务查询、任务进度或服务控制入口。

插件实现方负责让自身任务（task）、服务调用（service invocation）及相关资源真正退出；不得在推进这些工作的唯一执行线程（executor thread）上阻塞等待其完成。本库调用标准关闭并依据结果及受控代码持有推进最终卸载（unloading），不枚举或关闭业务服务（business service）。静态登记（static registration）可以被发现，不代表最终卸载（unloading）已经安全。

后续可将 `pi_db_server` 等既有库包装为合规外部产物，并作为真实集成示例（integration example）。包装方应尽量保持原有接口（API），同时完整保持全部服务接口（service API）、语义（semantics）和逻辑（logic）；当前尚未验证其完整初始化与退出路径。
