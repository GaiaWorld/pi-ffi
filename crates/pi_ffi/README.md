# pi_ffi

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

库清单目前无公开功能开关（features），直接依赖默认配置即可。Linux 和 Windows 随编译目标分配平台依赖。运行时可选和静态服务选择属于初始化及部署配置，依实际终端、插件和配套框架使用。

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

## 每轮初始化、关闭与释放（Initialization and cleanup）

宿主以经配套核实的真实 P2P 终端建立 `TerminalInput<T>`。只有在原生终端类型、实例、Rust 依赖和对应代码能保持到最后一次使用及释放时，调用 `unsafe { TerminalInput::new(terminal_arc) }`。宿主交出一份 `Config`，可用 `prepare_without_runtime` 省略运行时，或用 `prepare` 接收 `Option<Arc<R>>`。可选实例如有值须满足 `IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static` 的实际 Rust 合同。

```no_run
use pi_ffi::{
    config::Config,
    host::{Initialization, Manager, Plugin, PluginPackage, PluginRequest, TerminalInput},
};
use pi_result::Result;

fn prepare_round<T: Send + Sync + 'static>(
    manager: &Manager,
    package: &PluginPackage,
    request: &PluginRequest,
    terminal: TerminalInput<T>,
    config: Config,
) -> Result<(Plugin, Initialization)> {
    // terminal 来自实际宿主的配套构建和原生 P2P 终端。
    let plugin = manager.load(package, request)?;
    let round = plugin.prepare_without_runtime(terminal, config)?;
    // 持有原轮句柄，后面的初始化即使失败也能继续关闭这个轮次。
    Ok((plugin, round))
}
```

- `Initialization::id` 读取既有轮次标识。`initialize` 发起一次实际初始化，同轮重试共享确认结果，竞争返回 `Busy`。
- `Plugin::current_initialization` 可查询本模块仍需处理的原轮，包括初始化失败后应清理的轮次。同一 UUID 与完整版本在该进程最多有一个实际初始化实例。准备或清理未完成时，原轮仍可能保留这一占用。
- `Initialization::close` 结束原轮。若有必要工作未退出，处理真实 `pi_result`，保留原轮及其进度。也可在尚未调用初始化时取消准备的轮次。
- `Manager::maintain(budget)` 有界推进已登记的归还。`Plugin::unload` 显式归还本库的实际系统引用，须等原轮、受保护的调用以及相关引用退出。报告系统释放结果不明时沿保存的实际状态处理；宿主仍可读取已有元信息副本。

## 封装插件的三个入口（Plugin wrapping）

`plugin::Declaration` 提供跨版本稳定 UUID 和完整版本。`Exports<S>::new` 建立这一实际模块的一份标准接入，`S: Send + 'static` 是插件自有的本轮状态。`Inputs<T, R>` 提供原生终端、配置和可选运行时。

| 入口 | 给包装者的合同 |
| --- | --- |
| `Exports::metadata` | 为受保护调用写出编译期元信息。调用者提供有效的独占输出槽。 |
| `Exports::initialize` | 按实际类型接收 `Inputs<T,R>`，初始化函数用 `&mut Option<S>` 保存本轮效果及清理所需数据。 |
| `Exports::initialize_without_runtime` | 对应只接收运行时缺省的插件形态，终端与配置必传。 |
| `Exports::close` | 按原轮标识调用 Rust 关闭函数，清理插件本轮服务和工作。未完成时以 `pi_result` 报告，保留实际进度。 |
| `pi_ffi::abi` | 标准 C ABI 名称、布局、函数指针及初始化进度，分别用于三项固定导出。 |
| `pi_ffi::{config,runtime,service}` | 重导出 `pi_ffi_adapters` 的原生输入及静态服务接入，按其入口文档使用。 |

### 手工接入（Manual wrapping）

外部包装库使用 `cdylib`，依赖与宿主配套的 `pi_ffi`、`pi_result` 及 `pi_p2p_service`。示例保留收到的两项输入，省略运行时。三个 C 包装共用一个 `Exports`；真实插件在产生资源或登记前建立本轮清理状态。

```ignore
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

同一个插件制品可改用 `pi_ffi_macros::export_plugin!`，用相同声明、初始化和关闭函数生成三个 C 导出及类型检查。一个制品选择一套导出。

### 使用声明、配置和关闭（Combining capabilities）

插件通过 `service::register_static` 在标准初始化期间汇总本插件的适用声明并交配套框架登记。使用共享终端前须核实框架可一次登记、按本轮标识撤回并确认引用退出；由插件保存本轮记录并负责关闭。动态服务和业务 RPC 沿外部接口。原生终端句柄、引用和回调依对应的 `Terminal` 持有，先收尾这些使用才确认关闭。

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
