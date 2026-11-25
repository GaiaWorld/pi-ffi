# pi_ffi_adapters

## 用途与支持环境（Scope and support）

`pi_ffi_adapters` 安全交接 Rust 插件动态库（plugin dynamic library）初始化所需的四类原生输入。配置（Config）、P2P 终端（P2P service terminal）和初始化数据（initialization data）D 必传，运行时（runtime）可选。用于实现宿主（host）和插件的标准初始化接入，普通调用方可通过 `pi_ffi` 使用这些类型。

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
共享由具体 D 决定，例如 `Arc<T>`；无内容传 `()`，仍有合法第四输入。
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
