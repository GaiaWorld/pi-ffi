# pi-ffi

`pi-ffi` 是面向跨平台（cross-platform）、多进程（multi-process）插件系统（plugin system）的上游支持库，负责查找、识别、加载、管理和卸载符合约定的 Rust 插件动态库（plugin dynamic library），并支持服务接入与运行时（runtime）替换。

**当前处于设计阶段，尚无可用的完整接入接口（API）。** 下文区分已经对齐的使用约定和仍需设计的具体表示；已有 Rust、Cargo 与 `pi_result` 接口（API）的示例可以单独验证，不能据此推断本项目已经实现或通过验收。

## 支持范围与职责

首批正式支持与原生验证（native validation）的设计目标为 Windows x86_64／MSVC 和 Linux x86_64／GNU。这里声明的是目标范围，尚未完成这两个平台的产品验证。

本轮仅接纳符合本项目标准的 Rust 插件动态库（plugin dynamic library）。本项目提供下游实现插件所需的底层支持；符合标准的外部产物由下游提供。

| 参与方 | 负责内容 |
| --- | --- |
| 宿主（host） | 选择可信且遵守约定的外部产物，提供运行时（runtime）、终端相关实例及初始化配置（initialization configuration）；通过既有服务框架提供发现、访问和跨进程通信（inter-process communication） |
| 本库 | 在合法外部函数接口（FFI）调用范围内管理加载、版本接纳、并发调用（concurrent invocation）、生命周期（lifecycle）及受控能力的交接与释放 |
| 插件动态库（plugin dynamic library）的实现方 | 提供标准入口，管理自身任务（task）、服务和资源；遵守初始化、幂等性（idempotency）、错误、恐慌（panic）及清理约定；声明并正确处理自己使用的共享资源 |

进程间通信（IPC）、QUIC 以及发现、连接和访问设施均由宿主（host）与既有服务框架提供，本项目不实现这些设施。对不可信原生代码（native code）的安全隔离（security isolation）也由外部宿主（host）承担。

动态服务（dynamic service）自动封装（automatic wrapping）及运行时（runtime）替换均属于本轮最小可行产品（MVP）目标。当前接入 `pi-async-rt`，设计必须承接后续 `pi-async-rt_v1.0` 的接口（API）和语义（semantics）差异。具体接入方式仍在设计中。服务接入尽量支持静态服务（static service），同时保留动态服务（dynamic service）的选择；不能仅凭产物是动态库（dynamic library）就改变原有服务形式。

## 标准入口与初始化输入

标准至少包含以下三类入口。具体名称、类型（type）、布局（layout）、所有权（ownership）交接及完整返回格式尚未冻结。

| 入口 | 已对齐的使用约定 |
| --- | --- |
| 只读元信息（read-only metadata）查询 | 提供稳定身份声明及完整语义化版本号（semantic version）；在合法加载期间的初始化前、中、后及关闭期间、关闭后都可查询，不以业务初始化为前提 |
| 初始化 | 接收运行时（runtime）的外部函数接口（FFI）封装、终端相关实例及 `pi_config::Config`；明确返回成功或失败，并交付本次初始化的标识（initialization identifier），包括需要继续清理的失败方向 |
| 关闭、清理与释放 | 接收本次初始化的标识（initialization identifier），明确返回成功或失败；旧标识（identifier）的延迟调用不能关闭新一轮使用 |

外部函数接口（FFI）采用目标平台的 C 应用二进制接口（C ABI）和受控表示。上述 `pi_config::Config` 是已经确定的输入对象；其跨库表示仍需设计，不能直接把任意 Rust 对象地址当作可互操作的参数。`repr(C)` 本身也不会使其内部嵌套的任意 Rust 类型（type）具备稳定表示。参见 [Rust 类型布局（type layout）说明](https://doc.rust-lang.org/reference/type-layout.html#representations)。

标准入口名称区分协议主版本（protocol major version）；同一主版本内固定调用约定（calling convention）及参数、返回值的布局（layout）。不兼容变化使用新的入口版本（entry version），它与外部产物自身的业务版本（business version）分别判断。正式名称及声明将在设计冻结后补充。

### 身份、版本与反复使用

- 稳定身份不因文件改名、复制或发布不同版本而改变。同一进程（process）中，相同身份与相同完整语义化版本号（semantic version）最多存在一个已经初始化的实例（instance）。
- 完整版本包含构建元信息（build metadata）；例如 `1.2.3+build.1` 与 `1.2.3+build.2` 在本项目中用于区分不同实例版本（instance version）。版本优先级（version precedence）、范围匹配（range matching）与实例区分分别处理，不能把它们混为同一比较规则。[语义化版本规范（Semantic Versioning）](https://semver.org/spec/v2.0.0.html)规定构建元信息（build metadata）不参与版本优先级（version precedence）。
- 初始化前检查所要求的版本范围（version range），不匹配则不进行业务初始化。具体范围语法尚未选定。系统加载可能已经执行构造器（constructor）；初始化前检查不表示此前没有执行外部代码。
- 同一进程（process）内重复加载同一实际模块（loaded module）时，共享其本轮实际初始化；输入冲突不能靠再次初始化解决。标准入口自身必须具有语义幂等性（semantic idempotency）。
- 关闭失败时保留必要的代码映射（code mapping）和同版本占用，仅在安全前提满足时继续尚未完成的清理。超时不等于后台工作已经停止。
- 再次初始化必须等本轮清理成功，且旧业务工作与调用资格已经退出或失效。最终卸载（unloading）还须等待所有需要代码映射（code mapping）的引用退出；只读查询及其返回数据也受此约束。
- 宿主（host）仍拥有其共享运行时（runtime）和终端；单个外部产物的关闭不能擅自关闭整个共享执行环境。

不同版本使用不同文件，并不能消除共享依赖（shared dependency）、符号（symbol）、配置（configuration）、文件、端口等资源冲突。本库对受控能力提供守护；实现方仍须正确获取、使用和释放其私有逻辑涉及的资源。具体逐项检查与守护边界仍需冻结。

释放系统加载句柄（loader handle）不保证所有平台立即完成物理卸载（physical unloading）或重置静态状态（static state）。参见 [Windows `FreeLibrary`](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-freelibrary) 与 [POSIX `dlclose`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/dlclose.html)。若反复使用确实遇到当前无法可靠解决的保留状态问题，将依据真实证据讨论限制；目前没有采用“同版本仅能使用一次”的普遍限制。

### 共享持有（shared holding）与释放责任

以下是已经对齐的使用约定，具体 Rust 接口（API）仍待完整设计冻结。真实对象留在来源侧；初始化的三项输入通过受控 C 表取得同一宿主管理接口（host management interface），不增加第四个初始化入参。普通使用者通过 Rust 封装访问，不直接还原另一构建中的 `Config`、`Arc` 或错误报告（error report）。

本地克隆（Clone）共享一个拥有者；需要另一份独立持有（independent holding）时，必须显式取得。每份独立持有在对外交付前，宿主（host）就已登记剩余释放义务及所需代码保活。最后一个本地拥有者结束时，只提交这条已有记录；提交不临时分配记录，也不执行来源回调（source callback）。宿主（host）随后推进真实释放，最终失败保留在管理状态与关闭结果中。

例如，最后一个本地拥有者销毁时，另一个线程仍在执行一次已经获准的调用：释放提交可以成功，但关闭仍须返回忙碌（Busy），直到该调用退出、真实清理完成。**提交成功、持有归还和清理完成分别判断。** 不能只依据 `Drop` 已执行就释放动态库（dynamic library）的代码映射（code mapping）。

后续正式接入示例将给出管理推进及最终关闭的实际调用；当前尚无可直接运行的本库接口（API）示例。安全 Rust 也允许通过 `mem::forget` 跳过析构（Drop），因此遗忘拥有者可能延后卸载（unloading），不能强制清零来绕过它。依据为 [Rust 关于遗忘与安全的说明](https://doc.rust-lang.org/1.100.0/std/mem/fn.forget.html#safety)。

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

本库全部错误定义和处理使用 `pi_result`。跨外部函数接口（FFI）时采用宿主自有诊断数据副本（host-owned diagnostic copy）的方向：来源方提取约定的详细信息，宿主（host）复制并构造自己拥有的错误报告（error report），原报告（report）与原内存由创建方释放。完整字段（field）、编码（encoding）及释放接口（release API）仍待冻结。

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

## 服务接入与清理注意事项

远程过程调用（RPC）必须异步（asynchronous）。标准导出层无需提供通用 Rust 异步计算（Future）或轮询（poll）／唤醒（wake-up）协议；异步逻辑（asynchronous logic）由内部 Rust 实现，使用初始化注入的运行时（runtime）及宿主（host）服务框架推进。内部运行时（runtime）封装仍须定义提交、完成和释放约定，不能由此省略跨库安全设计。

清理必须让已经提交的任务（task）、服务调用（service invocation）及相关资源真正退出；不得在推进这些工作的唯一执行线程（executor thread）上阻塞等待其完成。静态登记（static registration）可以被发现，不代表其调用、关闭及最终卸载（unloading）已经安全。

后续可将 `pi_db_server` 等既有库包装为合规外部产物，并作为真实集成示例（integration example）。包装方应尽量保持原有接口（API），同时完整保持全部服务接口（service API）、语义（semantics）和逻辑（logic）；当前尚未验证其完整初始化与退出路径。

随着正式接口（API）冻结和实现，将继续补齐宿主（host）接入、外部产物导出、配置（configuration）传递、两类服务接入、失败清理及反复使用的可运行示例。这里的说明和示例不替代尚未完成的设计冻结或产品验收。
