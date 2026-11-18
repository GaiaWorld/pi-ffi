//! 可选的运行时（runtime）输入交接及 Rust 使用。
//!
//! 宿主（host）与插件（plugin）须链接并装入同一份 `pi_ffi_adapters` Rust
//! 动态库制品（dylib），使用匹配的 Rust 工具链（toolchain）、目标（target）
//! 和共享依赖（dependency）。标准初始化前须确认可信制品的配套关系，
//! 不支持的组合经 `pi_result` 拒绝。宿主交接实际实例，插件经原生特征
//! （trait）使用。整个运行时的推进及关闭由其外部拥有方负责。

/* 内部设计，不进入 API docs。

发布。B5 已在进程权威内绑定初始化轮次和输入，向 publish 提供本轮 Connection。
在共享适配库附着 B1 来源。用 ExportedInterface::<Arc<R>>::new::<RuntimeInterface>(&RUNTIME_TABLE)
配对类型和只读表头，再用 Registry::export 登记宿主的 Arc<R>。注册的
清理回调只确认本条适配记录可释放，B1 在锁外归还实际 Arc。
成功先承接已登记的 Object；失败不发布输入，来源残留责任走 B1。宿主保留
PublishedRuntime 至本轮输入交接结束。标准初始化准入、轮次重试及全局导出归 B5。持有、同步范围和类型核验
只能在Rust调用已具备二进制兼容前提后执行。已登记的元信息和实际加载
关系是部署与B5检查的输入，具体识别方式由B5细化。

同步交接。with_input 先用 Object::with_borrowed 取得现有持有的读取保护。
CURRENT_INPUT 为每个线程单独的可空记录。记录含实际 Registry 的共享持有、
对象来源／槽位／代际、模块纪元／轮次和原持有号（LeaseV1）。用 try_with
取得线程局部值，替换时把旧记录交给当前栈上的 RestoreInput。交换结束后才
执行初始化闭包（closure）。闭包在插件代码及各实际输入仍受保护时，把
标准结果、诊断和已发生的进度存到本侧，再返回 pi_result::Result<()>。
不会在 RefCell 借用或登记表锁内运行闭包、调用插件，或者删除对象。
成功、返回错误或栈展开均先恢复上一记录，再归还 B1 输入读取保护。
RestoreInput 借用 try_with 提供的槽位，Drop 无须再次访问 TLS。
恢复时先结束短期可变借用，再释放换出的记录。嵌套只检查最内层，不向
外层搜索；完整身份相同代表同一合法输入，外层仍按自身作用域退出。
槽位不可用以 InvalidState 返回。其它闭包错误同样先保存为主错误，随后
显式归还 B1 保护；若归还也报错，按 B1 追加。Rust 栈展开通过已有
B1 守卫提交归还，最终失败和责任由宿主的已有记录承接。

接收。缺席直接返回 None。对有值情况，在当前线程读取最新记录，按 LeaseV1
全部字段核输入并克隆 Registry，结束 RefCell 借用。无匹配记录时返回 Conflict，
线程局部存储已销毁时返回 InvalidState。先于接管持有。然后先以 Connection::initialization 核已引导的合法连接轮次，错轮报
Conflict。用已证当前调用输入及其保护走 Object::retain_borrowed，由 B1
核实际管理权威，再以 Object::interface 查询唯一 RuntimeInterface。
只在 Interface::with_call 获得本次 READ 权限后，调用实际
Registry::with_access::<Arc<R>, ()> 校验类型。Object 在其它加工前接管独立
持有，失败走已登记的归还途径。成功形成 Runtime<R>。初始化时的记录、表头
或类型名都不能授予下一次访问，真实 Arc<R> 从未作为 C 数据传输。

使用。with_runtime 先经 Interface::with_call 取得 USE 及代码保护，再用该
调用的 LeaseV1 向同一个 Registry 取得 USE 访问。短期构造 RuntimeView，借用
真实 R。Rust 闭包最多进入一次。把正常返回的 V 移入本次 Rust 栈中的
Option<V>，给两个 B1 访问层只返回单位值。先结束来源访问，再归还宿主调用。
B1 失败及追加的归还诊断均原样保留；已交付的 V 与之同时返回。正常契约下，闭包未进入时没有 V。B1 反常报告成功但未交付 V 时，
报 Corrupted。闭包恐慌（panic）按 Rust 规则展开，来源访问和宿主保活守卫依 B1
归还，标准 C 入口须在自己的 Rust 产物内捕获。禁止因归还报错而重放已发生的
操作。外层 Interface::with_call 经过共同宿主管理 C 表处理进入及归还。
内层 Registry::with_access 在来源侧借用 Rust 对象。取得 USE 后才调用
原生运行时方法。
V 是插件 Rust 持有的原值，可含调用前已存在的本地借用。类型绑定禁止
借自本次 RuntimeView 的引用逸出。没有插件工作状态的记录。

转发。同名原生方法只调用原 R 一次，同步借用不延长到这些方法返回后的
插件工作；类型、关联类型（associated type）和结果按原方法传递，覆写过的
默认方法也会命中提供方覆写。两个不带 self 的 IdleDispatch 静态入口按
提供方当前合法运行环境调用，同一注入实例的保证适用于带 self 的方法。有效上下文及原生结果
后续如何使用、清理，由插件代码遵守运行时自己的约定。输入封装没有借出 R
的出口，不授予共享运行时关闭权。

退出。Runtime 的本地 Clone 只克隆 B1 Interface 及同一 Registry 持有。
自动销毁按声明顺序先交回 Interface 的本地共享拥有者，再结束 Registry 的
本地引用。最后一份 Interface 持有依 B1 提交预登记释放记录；运行时真正
拥有方的实例及驱动继续独立存在。B5 只据标准关闭结果和已登记的持有及
调用推进关闭。仍占用时按 B1 规则报告 Busy；收尾失败或效果未知时保留
实际状态及应留的代码。插件声明关闭成功前须已处理其自行交给外部运行时
的插件代码引用。下一轮可选择另一个匹配实例或无运行时，旧轮的输入和
独立持有不得访问新轮。
*/

#[path = "runtime_inner.rs"]
mod inner;

use pi_async_runtime_core::{
    task::{CreateError, SpawnError, SubmitError},
    AllSender, AnySender, CloseState, IdleState, OneSender, RuntimeId, RuntimeStatus, Sleep,
    TaskConfig, TaskHandle, Timeout, TimeoutOn, UnsubmittedTask, WaitAllTimeout, WaitAnyTimeout,
    WaitOneTimeout,
};
pub use pi_async_runtime_core::{
    IdleDispatch, RuntimeQuery, RuntimeTime, RuntimeWait, TaskDispatch,
};
use pi_ffi_abi::{ApiHeaderV1, InterfaceRequestV1, LeaseV1, ObjectRefV1, UuidV1};
use pi_ffi_core::{
    control::Connection,
    object::{AbiInterface, Interface, Object},
    source::Registry,
};
use pi_result::{Error, ErrorKind, RawResult, Result};
use std::{cell::RefCell, future::Future, marker::PhantomData, sync::Arc, time::Duration};

// 只识别运行时（runtime）输入类型。C 表仅含版本及尺寸。
struct RuntimeInterface;

#[repr(C)]
struct RuntimeTable {
    header: ApiHeaderV1,
}

const READ: u64 = 1;
const USE: u64 = 2;

// 安全性（safety）：RuntimeTable 为 C 表示，表头固定且没有可调用槽位。
// 接口身份专属当前契约，校验和实际对象访问沿 B1 完成。
unsafe impl AbiInterface for RuntimeInterface {
    type Table = RuntimeTable;
    const ID: UuidV1 = UuidV1 {
        bytes: [
            0x57, 0x03, 0xc9, 0xa8, 0x0c, 0x14, 0x40, 0xfa, 0xa2, 0x69, 0xbc, 0x20, 0x18, 0xc9,
            0x9b, 0x07,
        ],
    };
    const MAJOR: u32 = 1;
    const SUPPORTED_RIGHTS: u64 = READ | USE;
}

const RUNTIME_REQUEST: InterfaceRequestV1 = InterfaceRequestV1 {
    interface_id: RuntimeInterface::ID,
    major: RuntimeInterface::MAJOR,
    size: std::mem::size_of::<RuntimeTable>() as u32,
};

static RUNTIME_TABLE: RuntimeTable = RuntimeTable {
    header: ApiHeaderV1 {
        major: RUNTIME_REQUEST.major,
        size: RUNTIME_REQUEST.size,
    },
};

struct InitializationInput {
    lease: LeaseV1,
    source: Arc<Registry>,
}

thread_local! {
    static CURRENT_INPUT: RefCell<Option<InitializationInput>> = const { RefCell::new(None) };
}

struct RestoreInput<'scope> {
    slot: &'scope RefCell<Option<InitializationInput>>,
    previous: Option<InitializationInput>,
}

/// 已登记的宿主（host）运行时（runtime）输入，绑定到一个初始化轮次。
///
/// 登记共享 `Arc<R>`。宿主（host）保持原实例的拥有权及运行责任。
pub struct PublishedRuntime {
    object: Object,
    source: Arc<Registry>,
}

impl PublishedRuntime {
    /// 在指定轮次登记宿主（host）已有的运行时（runtime）实例。
    ///
    /// `connection` 必须是标准初始化管理流程为该轮建立的连接。
    /// 源对象登记和首次持有准备失败时返回 `pi_result`，已登记的责任按 B1 归还。
    /// 成功后每次借出输入均沿本对象原有的读取权限。
    // @trace 1
    pub fn publish<R>(connection: Connection, runtime: Arc<R>) -> Result<Self>
    where
        R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
    {
        inner::publish(connection, runtime)
    }

    /// 为一次同步初始化调用临时借出运行时（runtime）的受控 C 输入。
    ///
    /// 调用方应持有本轮标准初始化准入资格和插件代码保护。接收须在本次调用
    /// 的线程完成，已接管的插件持有可跨线程使用。调用方在闭包（closure）内
    /// 接管诊断并记下标准调用的实际结果。闭包至多执行一次，栈展开（stack
    /// unwinding）按 Rust 规则传播。保护归还失败时仍须保留已发生的调用进度。
    // @trace 2
    pub fn with_input(
        &self,
        invoke: impl for<'call> FnOnce(&'call ObjectRefV1) -> Result<()>,
    ) -> Result<()> {
        self.object.with_borrowed(|input| {
            CURRENT_INPUT
                .try_with(|slot| {
                    let _restore = inner::install_input(self, input, slot);
                    invoke(input)
                })
                .map_err(|_| Error::new(ErrorKind::InvalidState))?
        })
    }
}

/// 插件（plugin）持有的已注入运行时（runtime）访问能力。
///
/// 本地克隆（clone）共享同一份独立持有。最后一份拥有者销毁后按共同合同
/// 归还持有。封装可跨线程使用，每次访问仍须取得当次权限。插件确认
/// 关闭成功前须确保其交给运行时的插件代码引用均已归还。
pub struct Runtime<R> {
    interface: Interface<RuntimeInterface>,
    source: Arc<Registry>,
    runtime: PhantomData<fn() -> R>,
}

/// 一次 Rust 调用内对原实例的受控借用（borrow），提供五项原生特征（trait）。
///
/// 借用只限 `Runtime::with_runtime` 的闭包范围。参数、返回值和关联类型
/// （associated type）沿用提供方。提供方原生结果可由插件（plugin）继续使用。
/// 带 `self` 的方法使用注入实例。`IdleDispatch` 的两个静态本地方法按
/// 提供方的合法当前运行环境执行。共享运行时的关闭权限留在外部拥有方。
pub struct RuntimeView<'access, R> {
    runtime: &'access R,
}

impl<R> Runtime<R>
where
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    /// 从同步初始化的当前输入取得插件（plugin）的独立持有。
    ///
    /// 缺席时返回 `Ok(None)`。有值时核原输入身份、当前管理连接、权限以及
    /// 真实类型 `R`。错线程、错轮次、类型不匹配或独立持有建立失败均通过
    /// `pi_result` 报告。已取得但未交付的持有按共同合同归还。
    ///
    /// # Safety
    ///
    /// `input` 有值时，必须来自本库合法的同步初始化借用。调用完成前，
    /// 外层须保护该输入的原持有、来源代码、上下文和诊断表，C 表符合共同
    /// 合同。`connection` 必须由合法配置（Config）引导建立且仍有效。
    /// 线程、轮次及实际管理权威是接收时的检查条件。两侧须已满足 Rust
    /// 配套制品和实际链接（linking）的使用前提。
    // @trace 3
    pub unsafe fn from_optional_input(
        connection: &Connection,
        input: Option<&ObjectRefV1>,
    ) -> Result<Option<Self>> {
        let Some(input) = input else {
            return Ok(None);
        };
        let source = inner::source_for_input(connection, input)?;
        // 安全性（safety）：外层保护真实输入。上一操作已核本次同步输入与
        // 合法连接的轮次，B1 接管会核实际管理权威及完整来源。
        let object = unsafe { Object::retain_borrowed(connection, input) }?;
        let interface = object.interface::<RuntimeInterface>()?;
        interface.with_call(READ, |call| {
            source.with_access::<Arc<R>, ()>(call.lease(), RUNTIME_REQUEST, READ, |_| Ok(()))
        })?;
        Ok(Some(Self {
            interface,
            source,
            runtime: PhantomData,
        }))
    }

    /// 在 Rust 闭包（closure）内使用宿主（host）提供的实际运行时（runtime）。
    ///
    /// `Ok(output)` 表明闭包正常返回且本次保护已归还。`Err((error, None))`
    /// 表明访问未建立，闭包未进入。`Err((error, Some(output)))` 表明已有
    /// 原始返回值，保护归还失败，原值仍归调用方。调用方按已发生的操作处理
    /// 该错误。若原值本身是运行时原生的结果（Result），其含义与类型保持原样。
    /// 闭包恐慌（panic）按 Rust 规则传播。借自本次访问的引用不能随返回值带出。
    // @trace 4
    pub fn with_runtime<V>(
        &self,
        use_runtime: impl for<'access> FnOnce(RuntimeView<'access, R>) -> V,
    ) -> RawResult<V, (Error, Option<V>)> {
        let mut output = None;
        let result = self.interface.with_call(USE, |call| {
            self.source
                .with_access::<Arc<R>, ()>(call.lease(), RUNTIME_REQUEST, USE, |runtime| {
                    output = Some(use_runtime(RuntimeView {
                        runtime: runtime.as_ref(),
                    }));
                    Ok(())
                })
        });
        inner::with_runtime_result(result, output)
    }
}

impl<R> Clone for Runtime<R> {
    // @trace 5
    fn clone(&self) -> Self {
        inner::clone_runtime(self)
    }
}

impl Drop for RestoreInput<'_> {
    // @trace 6
    fn drop(&mut self) {
        inner::restore_input(self)
    }
}

// 编译期要求（compile-time requirement）以实际字段的自动特征为依据。
struct Concurrent<T: Send + Sync>(PhantomData<T>);
const _: Option<Concurrent<PublishedRuntime>> = None;
const _: Option<Concurrent<Runtime<()>>> = None;

impl<R: TaskDispatch> TaskDispatch for RuntimeView<'_, R> {
    type Notifier = R::Notifier;

    // @trace 7
    fn create_with_config<F, M>(
        &self,
        future: F,
        config: TaskConfig<M>,
    ) -> RawResult<(UnsubmittedTask, TaskHandle<Self::Notifier, F::Output, M>), CreateError<F, M>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        M: Send + Sync + 'static,
    {
        inner::view_create_with_config::<R, F, M>(self.runtime, future, config)
    }

    // @trace 8
    fn submit(&self, task: UnsubmittedTask) -> RawResult<(), SubmitError> {
        inner::view_submit::<R>(self.runtime, task)
    }

    // @trace 9
    fn create<F>(
        &self,
        future: F,
    ) -> RawResult<(UnsubmittedTask, TaskHandle<Self::Notifier, F::Output>), CreateError<F, ()>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        inner::view_create::<R, F>(self.runtime, future)
    }

    // @trace 10
    fn spawn_with_config<F, M>(
        &self,
        future: F,
        config: TaskConfig<M>,
    ) -> RawResult<TaskHandle<Self::Notifier, F::Output, M>, SpawnError<F, Self::Notifier, M>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        M: Send + Sync + 'static,
    {
        inner::view_spawn_with_config::<R, F, M>(self.runtime, future, config)
    }

    // @trace 11
    fn spawn<F>(
        &self,
        future: F,
    ) -> RawResult<TaskHandle<Self::Notifier, F::Output>, SpawnError<F, Self::Notifier>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        inner::view_spawn::<R, F>(self.runtime, future)
    }
}

impl<R: IdleDispatch> IdleDispatch for RuntimeView<'_, R> {
    // @trace 12
    fn submit_idle(&self, task: UnsubmittedTask) -> RawResult<(), SubmitError> {
        inner::view_submit_idle::<R>(self.runtime, task)
    }

    // @trace 13
    fn idle_with_config<F, M>(
        &self,
        future: F,
        config: TaskConfig<M>,
    ) -> RawResult<TaskHandle<Self::Notifier, F::Output, M>, SpawnError<F, Self::Notifier, M>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        M: Send + Sync + 'static,
    {
        inner::view_idle_with_config::<R, F, M>(self.runtime, future, config)
    }

    // @trace 14
    fn idle<F>(
        &self,
        future: F,
    ) -> RawResult<TaskHandle<Self::Notifier, F::Output>, SpawnError<F, Self::Notifier>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        inner::view_idle::<R, F>(self.runtime, future)
    }

    // @trace 15
    fn idle_local_with_config<F, M>(
        future: F,
        config: TaskConfig<M>,
    ) -> RawResult<TaskHandle<Self::Notifier, F::Output, M>, SpawnError<F, Self::Notifier, M>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        M: Send + Sync + 'static,
    {
        inner::view_idle_local_with_config::<R, F, M>(future, config)
    }

    // @trace 16
    fn idle_local<F>(
        future: F,
    ) -> RawResult<TaskHandle<Self::Notifier, F::Output>, SpawnError<F, Self::Notifier>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        inner::view_idle_local::<R, F>(future)
    }
}

impl<R: RuntimeTime> RuntimeTime for RuntimeView<'_, R> {
    type Timer = R::Timer;

    // @trace 17
    fn timeout(&self, duration: Duration) -> Timeout<Self::Timer> {
        inner::view_timeout::<R>(self.runtime, duration)
    }

    // @trace 18
    fn timeout_on<F: Future>(&self, future: F, duration: Duration) -> TimeoutOn<Self::Timer, F> {
        inner::view_timeout_on::<R, F>(self.runtime, future, duration)
    }

    // @trace 19
    fn sleep(&self, duration: Duration) -> Sleep<Self::Timer> {
        inner::view_sleep::<R>(self.runtime, duration)
    }
}

impl<R: RuntimeWait> RuntimeWait for RuntimeView<'_, R> {
    // @trace 20
    fn wait_one_timeout<T>(
        &self,
        duration: Duration,
    ) -> (WaitOneTimeout<Self::Timer, T>, OneSender<T>) {
        inner::view_wait_one_timeout::<R, T>(self.runtime, duration)
    }

    // @trace 21
    fn wait_any_timeout<T>(
        &self,
        duration: Duration,
    ) -> (WaitAnyTimeout<Self::Timer, T>, AnySender<T>) {
        inner::view_wait_any_timeout::<R, T>(self.runtime, duration)
    }

    // @trace 22
    fn wait_all_timeout<T>(
        &self,
        duration: Duration,
    ) -> (WaitAllTimeout<Self::Timer, T>, AllSender<T>) {
        inner::view_wait_all_timeout::<R, T>(self.runtime, duration)
    }
}

impl<R: RuntimeQuery> RuntimeQuery for RuntimeView<'_, R> {
    // @trace 23
    fn id(&self) -> RuntimeId {
        inner::view_id::<R>(self.runtime)
    }

    // @trace 24
    fn status(&self) -> RuntimeStatus {
        inner::view_status::<R>(self.runtime)
    }

    // @trace 25
    fn is_driving(&self) -> bool {
        inner::view_is_driving::<R>(self.runtime)
    }

    // @trace 26
    fn last_idle_state(&self) -> IdleState {
        inner::view_last_idle_state::<R>(self.runtime)
    }

    // @trace 27
    fn close_state(&self) -> CloseState {
        inner::view_close_state::<R>(self.runtime)
    }

    // @trace 28
    fn external_task_count(&self) -> usize {
        inner::view_external_task_count::<R>(self.runtime)
    }

    // @trace 29
    fn internal_task_count(&self) -> usize {
        inner::view_internal_task_count::<R>(self.runtime)
    }

    // @trace 30
    fn timer_registration_count(&self) -> usize {
        inner::view_timer_registration_count::<R>(self.runtime)
    }

    // @trace 31
    fn idle_task_count(&self) -> usize {
        inner::view_idle_task_count::<R>(self.runtime)
    }

    // @trace 32
    fn in_flight_task_count(&self) -> usize {
        inner::view_in_flight_task_count::<R>(self.runtime)
    }

    // @trace 33
    fn waiting_for_wake_count(&self) -> usize {
        inner::view_waiting_for_wake_count::<R>(self.runtime)
    }

    // @trace 34
    fn pending_cancel_task_count(&self) -> usize {
        inner::view_pending_cancel_task_count::<R>(self.runtime)
    }
}
