# pi-ffi

[接口版本、逐版接入和完整历史](#interface-versions-and-compatibility)。

`pi-ffi` 是面向跨平台（cross-platform）、多进程（multi-process）插件系统（plugin system）的上游支持库，负责查找、识别、加载、初始化、维护、关闭和卸载符合约定的 Rust 插件动态库（plugin dynamic library），安全交接初始化入参并支持运行时（runtime）替换。本库提供插件在标准初始化时汇总并登记其静态服务（static service）的接入。

**使用状态。** 两平台的完整插件运行能力尚未验证。

工作区（workspace）各库的职责如下。

| 库（crate） | 当前职责 |
| --- | --- |
| [pi_ffi_abi](crates/pi_ffi_abi/README.md) | 固定 C 表示、身份／轮次／持有、诊断和共同操作表；不依赖业务库 |
| [pi_ffi_core](crates/pi_ffi_core/README.md) | 受控对象交接、调用保护、错误诊断及清理 |
| [pi_ffi_adapters](crates/pi_ffi_adapters/README.md) | 运行时（runtime）、配置（Config）和 P2P 终端（terminal）的接入 |
| [pi_ffi](crates/pi_ffi/README.md) | 宿主（host）管理和插件的三类标准接口 |
| [pi_ffi_macros](crates/pi_ffi_macros/README.md) | 可选过程宏（procedural macro），在编译期生成标准导出 |

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

宿主通过 `pi-async-runtime-core` 的标准特征（trait）提供运行时（runtime）。满足合同的线程安全共享实例可以来自阻塞运行时（blocking runtime）或运行时池（runtime pool）。本库为插件封装共享和克隆（clone）。原实例只需满足下列特征及线程安全要求。

提供运行时（runtime）时，所需能力为普通任务派发（TaskDispatch）、空闲派发（IdleDispatch）、计时（RuntimeTime）、等待（RuntimeWait）及查询（RuntimeQuery）。插件普通任务（task）的异步计算（Future）按原生 Rust 派发合同使用 `Send + 'static`，不额外要求 `Sync` 或 `Unpin`。本地任务派发（LocalTaskDispatch）用于原有本地上下文（context），合法条件由运行时库（runtime library）规定。空闲提交、等待、取消请求（cancellation request）及结果处理沿用运行时库的 Rust 接口（API）。

每轮初始化固定运行时（runtime）的有无及所提供的实例。新轮或新版本可以提供另一实例，也可以省略该输入。在途异步计算（Future）仍在原实例完成或收尾。外部运行时（runtime）的实际拥有方负责创建、推进、保持可用和关闭实例。

本库经安全 Rust 封装透传外部运行时（runtime）的有无。提供实例时，宿主（host）与插件（plugin）使用同一运行时库（runtime library）及其匹配的 Rust 接口（API）。pi-ffi 负责将宿主提供的实际实例安全交接给插件。插件动态库（plugin dynamic library）自行使用该实例并在关闭时收尾自己的工作。C 接口（C API）只用于 pi-ffi 的外部函数接口（FFI）边界。

本库依据插件的标准关闭结果及相关跨库引用判断是否可卸载（unloadable）。共享运行时（runtime）可继续为其他使用者工作。若仍有在途调用（in-flight call）或引用依赖插件代码，本库保留相应映射并按约定返回 Busy 或失败。运行时查询（RuntimeQuery）供插件自己的 Rust 代码显式调用。

## 标准入口与初始化输入

宿主（host）和插件（plugin）均使用 Rust 实现。pi-ffi 定义以下三类同步管理接口的签名（signature）和语义（semantics），默认使用表中最新组。插件动态库（plugin dynamic library）用 Rust 实现并显式导出所选规范对应的 C 接口（C API）。封装者按本库的标准、规范和公开 Rust 接口（API）手工接入，或用可选过程宏（procedural macro）生成所选版本的准确导出。业务服务（business service）通过既有服务框架提供。

| 入口 | 已对齐的使用约定 |
| --- | --- |
| `pi_ffi_metadata_v1` 只读元信息（read-only metadata） | 提供稳定身份声明及完整语义化版本号（semantic version）；在合法加载期间的初始化前、中、后及关闭期间、关闭后都可查询，不以业务初始化为前提 |
| `pi_ffi_initialize_v2` 初始化 | 按运行时、终端、配置、初始化数据（initialization data）顺序接收四个受控对象指针；仅运行时可为空，数据为 `()` 时也必传。返回实际初始化进度及共同结果，按原轮标识继续清理 |
| `pi_ffi_close_v1` 关闭 | 接收本次初始化的标识（initialization identifier），明确返回成功或失败；旧标识（identifier）的延迟调用不能关闭新一轮使用 |

外部函数接口（FFI）采用目标平台的 C 应用二进制接口（C ABI）和受控表示。运行时（runtime）可省略，终端（terminal）和配置（Config）必传；默认初始化 v2 还必传初始化数据（initialization data）D，v1 没有 D。由标准 Rust 接入核对本轮登记并接收真实对象，`Config` 与 D 在同一轮分别只移交一次。`repr(C)` 本身也不会使其内部嵌套的任意 Rust 类型（type）具备稳定表示。参见 [Rust 类型布局（type layout）说明](https://doc.rust-lang.org/reference/type-layout.html#representations)。

默认初始化 v2 的 Rust 宿主（host）使用 `prepare(terminal, config, runtime, data)` 或
`prepare_without_runtime(terminal, config, data)`，插件（plugin）通过
`Inputs<T,R,D>::data` 接收原 D。只要求 `D: Send + Sync + 'static`，不要求 `Clone`。
双方约定 `D = Arc<T>` 时可共享同一实际对象；无内容显式传 `()`。类型定义、配套
Rust 构建（matching build）、分配器（allocator）及析构／回调代码存续须由双方核实。
迁移至 v2 时，旧调用补末尾参数，手工导出改为四指针 `pi_ffi_initialize_v2`／`InitializeFnV2`；
过程宏（procedural macro）从初始化函数推断 D，不新增字段。旧 v1 声明保留，新装载器
（loader）不回退。完整可编译示例与迁移说明见 [pi_ffi](crates/pi_ffi/README.md) 和
[pi_ffi_macros](crates/pi_ffi_macros/README.md)。`pi_result` 按代码需要显式依赖，使用宏时
另外依赖 `pi_ffi_macros`；`pi_ffi` 重导出原生适配模块（adapter modules），不重导出过程宏。

标准入口名称区分协议主版本（protocol major version）；同一主版本内固定调用约定（calling convention）及参数、返回值的布局（layout）。不兼容变化使用新的入口版本（entry version），它与外部产物自身的业务版本（business version）分别判断。完整声明见 [三类标准 C 接口](crates/pi_ffi_abi/src/management.rs)。

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

## 选择接入方法

宿主（host）使用 `pi_ffi::host` 装入可信插件包（plugin package），提供原生 P2P 终端（terminal）、配置（Config）和可选运行时（runtime），初始化 v2 另提供初始化数据（initialization data）D。载入前核对实际发布制品和依赖，保留原轮次句柄用于失败后的关闭。关闭完成并确认引用退出后，再按包的选择退役本库的系统引用。 [宿主和手工封装示例](crates/pi_ffi/README.md)。

包装库使用 Rust `cdylib`。手工接法用 `pi_ffi::plugin::Exports` 编写所选版本的标准 C 导出，可选 `export_plugin!` 生成相同接线。一个插件产物可同时显式提供多个版本，各版共用同一初始化状态。 [可选宏及示例](crates/pi_ffi_macros/README.md)。

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

Rust 接口用 `pi_result` 报告操作的实际失败。宿主可保存自己的诊断副本，在插件释放后继续读取。独立诊断可保存原来源的 `user`、`verbose`、`debug` 三段文本；诊断不能建立时，可从结果中得知操作与诊断失败的分类。 [错误接入与诊断用法](crates/pi_ffi_core/README.md)。

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

插件通过既有服务框架提供异步远程过程调用（asynchronous RPC），其接口（API）和行为由插件及框架维护，对本库完全透明。插件使用既有运行时（runtime）的 Rust 接口推进内部异步计算。三类标准管理接口以同步方式调用。

插件实现方负责让自身任务（task）、服务调用（service invocation）及相关资源真正退出；不得在推进这些工作的唯一执行线程（executor thread）上阻塞等待其完成。本库调用标准关闭并依据结果及受控代码持有推进最终卸载（unloading），不枚举或关闭业务服务（business service）。静态登记（static registration）可以被发现，不代表最终卸载（unloading）已经安全。

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
