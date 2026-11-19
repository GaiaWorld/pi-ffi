//! 来源对象登记与清理（source object registration and cleanup）。

/* 内部设计，不进入API docs。
使用parking_lot 0.13互斥锁（Mutex）保护登记及内部管理状态。锁内只核对、预留、移交、计数和设置状态；不执行来源代码、外部回调（callback）、格式化或任何可能触发用户析构（Drop）的最后一次释放。

对象、持有、访问、调用及清理状态有不同的唯一拥有方：来源管理真实对象／持有／访问，宿主管理释放义务／调用代码保活。宿主释放记录不是另一份能独立宣布对象已销毁的计数。

| 操作 | 线性化位置（linearization point） | 失败后归属 |
| --- | --- | --- |
| 来源登记 | 宿主完整记录及代码持有提交 | 提交前外层调用仍持有代码；提交后来源上下文只由宿主退役程序释放 |
| 来源访问 | 同一来源锁内校验完成并增加活动访问 | 获准前无借用；获准后保留真实值直到访问归还 |
| 撤销 | 来源锁内禁止新业务 | 已获准访问完成，合法清理继续 |
| 持有发布 | 来源准备、宿主预登记均成功后提交来源活跃持有 | 不发布半成品；剩余准备载荷留在来源持续记录中 |
| 提交释放 | 宿主把既有记录设为待释放 | 本地拥有者不再承担后续责任；宿主持续推进 |
| 实际持有归还 | 来源把指定持有设为已归还 | 不等于真实值已经完成关闭或析构 |
| 最终析构 | 关闭成功后，锁外恰好执行一次真实值与回调捕获的析构 | 恐慌后保持最终失败，不盲目重析构 |

计数不回绕。来源槽位在本来源存续期内不复用、代际为1并严格校验；来源编号在宿主进程中不复用，因此旧槽位不会借重新加载复活。来源条目与操作记录使用可在副作用前`try_reserve`的容器；空间不足先拒绝，不靠之后补记录。

幂等记录在仍允许旧请求重放期间保存完整输入与结果；不以可能碰撞的哈希替代输入相等。内存成本随本轮记录增长，不能声称该安排天然有界或零成本。具体宿主容量、推进预算与回放记录回收由宿主管理实现提供；共同接口不得提前丢弃仍承诺可重放的结果。后续热路径（hot path）适配须另外证明成本，不能沿用冷路径（cold path）的结论。

`Registry::attach`先分配稳定来源上下文与共享状态，阶段为登记中，再调用宿主登记。来源号写入并完成本地发布前，控制回调不得宣称其可退役；未完成登记的清理责任不能因本地包装销毁而丢失。承诺转换恐慌（panic）的标准来源在有效构建不支持栈展开（stack unwinding）时拒绝登记。

导出过程的完整顺序：

1. 为对象、持有、必要去重及恢复位置预留容量；真实值始终有唯一明确归属。
2. 在来源锁内建立准备条目，包含本地类型、接口表、权限、关闭回调和初始持有；尚不对外交付。
3. 锁外向宿主预登记该持有的释放义务。
4. 登记成功后，在来源锁内提交活跃持有；之后不执行可能失败的外部处理再交付原始表示。
5. 构造本地安全拥有者。任何失败均不发布半成品；已预登记的剩余义务提交宿主，未预登记的准备载荷仍由来源上下文追踪并推进收尾。

`retain`先核原持有、轮次及新持有准入，再按操作标识准备记录。相同标识同输入重放活动结果；换输入冲突，已归还结果不复活。新持有同样先预登记宿主义务再返回。安全包装只接管一次重放成功的独立持有。

最后释放统一使用返回固定pi_result整数投影的无分配出口。`Drop`不构造Report；宿主持久记录最终失败。显式`Object::release`先用`Arc::try_unwrap`判断是否为最后本地拥有者，确需提交时使用同一有限出口，再在允许分配的显式返回路径形成pi_result。不能在Drop里先构造错误再忽略。

本地`Object::clone`只增加本地共享拥有者，不访问C表。`try_retain`才新建独立来源持有。`retain_for_transfer`生成不可克隆移交包装；交出前销毁则提交其已存在的释放义务。高级不安全（unsafe）接管入口要求调用者证明唯一拥有权，普通宿主接口不接收裸指针。

来源活动访问保留本地`Arc<Value>`，只在本来源把真实值解释为T。活动访问结束时先结束对T的借用、归还临时值持有，再减活动计数；来源登记仍拥有原值，不能在锁内意外触发最后析构。

每个有副作用的入口在执行可能恐慌的处理之前，建立持续可恢复的操作位置及作用域保护。恐慌发生后，已提交效果不回退成“从未执行”；未提交准备载荷不遗失，无法确认安全恢复的对象停止新业务并保留清理失败。不能只使用`AssertUnwindSafe`就声称业务状态恢复。

每个C入口最外层分别保护完整处理，包括输入检查、错误建立、文本格式化与释放。正常错误走`pi_result`，可捕获恐慌转为本地`pi_result`；最外层诊断建立也失败时返回约定的双分类分支。辅助整数状态入口返回有限分类，不递归分配来源错误。

恐慌负载（panic payload）只读取已知字符串类型，不调用任意格式化。其释放再次恐慌时，来源作用域仍须保留未知收尾及代码责任。显式终止、分配器终止与不可恢复二次恐慌按异常保证范围不保证恢复；不由宿主跨C捕获另一Rust产物的展开。

宿主收到释放提交后才执行来源`release`。同持有只归还一次；仍有活动访问时保存清理进度并返回确认，不等待回调退出。来源登记及宿主代码持有持续覆盖剩余步骤。

`maintain`每次最多推进一个已具备条件的对象：锁内取得唯一清理资格并移出关闭回调，锁外调用；Busy则把原值与回调还回条目并保留进度；其它错误保留原值、回调及失败，不自动重试不确定操作。成功后在锁外恰好析构真实值及关闭回调捕获；两者都确认成功后才标对象完成。未对外发布的准备载荷同样纳入这一流程。

清理回调在正常清理推进阶段执行，继续允许合法读取与清理重入。来源最终退役的控制调用独占阶段不执行需要再次访问本来源的业务收尾：先推进这些工作，再在无活动调用的独占检查中确认全部持有、访问、诊断和清理完成。避免“独占阻断回调所需清理、回调又等待该权限”的循环。

`try_retire`把来源设为关闭阶段并检查全部前提；未齐为Busy，不等待，不因旧的release成功而忽略新发现的失败。清理已齐后才进入上下文释放阶段。`release_context`只释放已清空用户资源、回调捕获及正常错误缓存的来源上下文；真实析构失败或结果不确定时不再调用该上下文，宿主保留映射及版本占用。

宿主只在回调已返回后退役来源登记。最终系统持有释放仍须等全部所需来源、任务、回调、诊断、私有工作及外层调用退出。标准接口不承诺物理卸载（physical unloading）或静态状态复位；安全遗忘持有者允许延后释放，不强制清零。
*/

#[path = "source_inner.rs"]
mod inner;

use crate::control::Connection;
use crate::diagnostic::{CachedFailure, Snapshot};
use crate::object::{AbiInterface, Object};
use core::any::{Any, TypeId};
use core::marker::PhantomData;
use parking_lot::Mutex;
use pi_ffi_abi::{
    ApiHeaderV1, DiagnosticApiV1, DiagnosticInfoV1, DiagnosticRefV1, InitIdV1, InterfaceRequestV1,
    InterfaceViewV1, LeaseV1, MaintenanceV1, ObjectKeyV1, ObjectRefV1, OperationIdV1, OutcomeV1,
    SourceApiV1, SourceControlApiV1,
};
use pi_result::{Error, Result};
use std::sync::Arc;

// 来源上下文的真实C分派连接（C dispatch binding）；表只读，代码随来源登记保活。
// @trace 1
pub(crate) static SOURCE_API: SourceApiV1 = SourceApiV1 {
    header: ApiHeaderV1 {
        major: 1,
        size: core::mem::size_of::<SourceApiV1>() as u32,
    },
    validate: Some(validate),
    retain: Some(retain),
    release: Some(release),
    query_interface: Some(query_interface),
};
// @trace 2
pub(crate) static DIAGNOSTIC_API: DiagnosticApiV1 = DiagnosticApiV1 {
    header: ApiHeaderV1 {
        major: 1,
        size: core::mem::size_of::<DiagnosticApiV1>() as u32,
    },
    info: Some(diagnostic_info),
    copy: Some(diagnostic_copy),
    release: Some(diagnostic_release),
};
// @trace 3
pub(crate) static SOURCE_CONTROL_API: SourceControlApiV1 = SourceControlApiV1 {
    header: ApiHeaderV1 {
        major: 1,
        size: core::mem::size_of::<SourceControlApiV1>() as u32,
    },
    maintain: Some(maintain),
    try_retire: Some(try_retire),
    release_context: Some(release_context),
};

/// 来源侧实际Rust对象的受控登记表（controlled source registry）。
/// 不实现Clone；来源持有、访问及宿主上下文分别维持内部共享状态。
// @trace 4
pub struct Registry {
    /// 来源上下文、活动访问和本地登记表共享状态。
    shared: Arc<Shared>,
}

/// 来源内部对象T可支持的一份固定接口声明（interface declaration）。
// @trace 5
pub struct ExportedInterface<T: Send + Sync + 'static> {
    /// 固定接口身份、主版本和准确尺寸。
    request: InterfaceRequestV1,
    /// 本来源只读操作表地址。
    table: ExportAddress,
    /// 本接口声明接受的共同权限位并集。
    supported_rights: u64,
    /// 把表与本来源T关联，不跨C传递T。
    marker: PhantomData<fn(&T)>,
}

// @trace 6
pub(crate) struct ExportAddress {
    /// 只由本来源登记并保活的只读表地址。
    pointer: *const core::ffi::c_void,
}
// @trace 7
pub(crate) struct Shared {
    /// 来源登记与释放义务归属的同一宿主连接。
    connection: Connection,
    /// 仅登记前写入一次；退役后不得再解引用。
    context: std::sync::OnceLock<SourceAddress>,
    /// 来源登记状态的唯一互斥保护。
    state: Mutex<State>,
}
/// 宿主登记拥有的稳定地址；真实结构仅在本来源解释。
// @trace 8
pub(crate) struct SourceContext {
    /// 宿主稳定上下文拥有的来源共享状态。
    shared: Arc<Shared>,
}
// @trace 9
pub(crate) struct SourceAddress {
    /// 宿主登记拥有的稳定地址，仅最终控制释放。
    pointer: *mut SourceContext,
}
// @trace 10
pub(crate) struct State {
    /// 宿主进程内不复用的来源编号，登记前为零。
    source: u64,
    /// 来源准入及退役阶段。
    phase: SourcePhase,
    /// 本来源不复用的下一个对象槽位，溢出拒绝。
    next_slot: u64,
    /// 本来源不复用的下一个独立持有号。
    next_holding: u64,
    // Vec可用try_reserve在副作用前准备容量；槽号不复用，代际固定为1并严格检查。
    // 操作按完整(actor,sequence)排序并比较完整输入，不使用哈希近似相等。
    /// 实际对象、诊断与未完成清理的持续记录。
    entries: Vec<Entry>,
    /// 按完整操作标识索引的恢复及幂等重放记录。
    operations: Vec<(OperationIdV1, Operation)>,
    /// 不依赖在失败时追加容器；最终失败不能被某次已确认的释放覆盖。
    failure: Option<Failure>,
}
// @trace 11
pub(crate) enum SourcePhase {
    /// 稳定上下文已建立但来源号尚未发布。
    Registering,
    /// 按具体权限接受持有及访问。
    Open,
    /// 停止新业务并继续允许合法清理。
    Closing,
    /// 执行最终独占检查，不执行需本来源重入的业务收尾。
    Retiring,
    /// 来源资源与上下文均已确认结束。
    Retired,
    /// 最终责任未清，保持代码及版本占用。
    Failed,
}
// @trace 12
pub(crate) struct Entry {
    /// 本来源对象的完整代际身份。
    key: ObjectKeyV1,
    /// 所属初始化轮次，不能被旧轮复用。
    initialization: InitIdV1,
    /// 对象准入、撤销、清理和最终状态。
    phase: EntryPhase,
    /// 共同权限并集；具体调用还须核准确接口。
    rights: u64,
    /// 已获准且尚未结束的来源访问数。
    active: u64,
    /// 各独立持有的来源状态与宿主登记确认。
    holdings: Vec<(u64, Holding)>,
    /// 真实对象或不可变诊断的唯一来源持有。
    value: Option<Arc<Value>>,
    /// 在状态锁内只移动，不调用或析构。
    close: Option<Box<dyn Fn(&Value) -> Result<()> + Send + Sync>>,
    /// 最终失败责任；未清除时不得假报退役。
    failure: Option<Failure>,
}
// @trace 13
pub(crate) enum EntryPhase {
    /// 对象与首持有未完成对外发布。
    Preparing,
    /// 按权限允许新访问。
    Live,
    /// 停止新业务，既有访问继续完成。
    Revoking,
    /// 唯一关闭尝试正在锁外执行。
    Closing,
    /// 关闭已确认成功，等待唯一最终析构。
    ReadyToDrop,
    /// 真实值及回调捕获正在锁外析构。
    Dropping,
    /// 两项析构均确认成功，无待清责任。
    Retired,
    /// 关闭或析构失败，不自动重跑不确定步骤。
    Failed,
}
// @trace 14
pub(crate) enum Value {
    /// 只由本来源解释的真实Rust对象及固定C接口。
    Object {
        type_id: TypeId,
        value: Arc<dyn Any + Send + Sync>,
        interfaces: Box<[(InterfaceRequestV1, ExportAddress, u64)]>,
    },
    /// 只读三文本诊断，独立持有归还。
    Diagnostic(Arc<Snapshot>),
}
// @trace 15
pub(crate) struct Holding {
    /// 独立持有的发布与归还阶段。
    phase: HoldingPhase,
    /// 来源记录与宿主释放记录都准备成功后才可交付。
    tracked: bool,
}
// @trace 16
pub(crate) struct Failure {
    /// 最终失败的主要pi_result分类。
    kind: pi_result::ErrorKind,
    /// 已有次级失败的公共分类。
    secondary: Option<pi_result::ErrorKind>,
    /// 只在来源内部保留的失败报告；存在时禁止成功退役。
    report: Option<Arc<Error>>,
}
// @trace 17
pub(crate) enum HoldingPhase {
    /// 来源持有正在准备，尚不可交付。
    Preparing,
    /// 来源与宿主记录均成功，可接管一次。
    Live,
    /// 归还请求已确认，剩余清理仍被追踪。
    ReleaseRequested,
    /// 该持有已归还，不可通过旧操作复活。
    Returned,
}
// @trace 18
pub(crate) struct Operation {
    /// 完整输入的本地值；禁止仅以可能碰撞的哈希判断相等。
    input: OperationInput,
    /// 本操作当前进度或可重放结果。
    phase: OperationPhase,
}
// @trace 19
pub(crate) enum OperationInput {
    /// 保存新持有请求的完整原lease。
    Retain(LeaseV1),
    /// 保存归还请求的完整目标lease。
    Release(LeaseV1),
    /// 本来源的一次有界清理推进。
    Maintain,
    /// 本来源的一次退役检查。
    Retire,
}
// @trace 20
pub(crate) enum OperationPhase {
    /// 同操作正在执行，重复调用返回Busy。
    Running,
    /// 已有进度但尚不能确认结束。
    Pending,
    // 回放由当前仍有效的来源上下文重建传输字段，不在状态中保存另一份拥有者。
    /// 保存仍活动的新持有字段，重放不增加拥有权。
    Retained(LeaseV1),
    /// 指定持有归还已确认。
    Released,
    /// 本次推进已完成的瞬时状态。
    Maintained(MaintenanceV1),
    /// 本次来源收尾已确认。
    Retired,
    /// 纯文本快照或双分类失败，可安全重放。
    Failed(CachedFailure),
}
/// 真实值的本次来源内部访问；不把T引用或锁守卫交出本次调用。
// @trace 21
pub(crate) struct Access {
    /// 活动访问期间保留的来源状态。
    shared: Arc<Shared>,
    /// 归还活动计数时核对的完整对象身份。
    key: ObjectKeyV1,
    /// 本次借用真实值的临时Arc，先于active归还。
    value: Option<Arc<Value>>,
    /// 已归还标记，避免重复扣减。
    finished: bool,
}

/// 关闭回调在锁外执行时的唯一资格；Busy或失败时须把载荷原样归回条目。
// @trace 22
pub(crate) struct CleanupAttempt {
    /// 清理期间持续追踪来源及代码责任。
    shared: Arc<Shared>,
    /// 获得唯一清理资格的对象身份。
    key: ObjectKeyV1,
    /// 锁外清理时保留的真实载荷。
    value: Arc<Value>,
    /// 移出的关闭回调；Busy时原样归还。
    close: Option<Box<dyn Fn(&Value) -> Result<()> + Send + Sync>>,
    /// 已提交确定结果；false时析构执行恢复。
    resolved: bool,
}

/// 同一来源操作的恢复资格，先建立再执行可能恐慌的处理。
// @trace 23
pub(crate) struct Mutation {
    /// 变更期间保持来源持续记录。
    shared: Arc<Shared>,
    /// 已预留的操作位置；纯对象准入可为空。
    operation: Option<OperationIdV1>,
    /// 关联对象身份；纯来源操作可为空。
    object: Option<ObjectKeyV1>,
    /// 已明确提交或安全归回，禁止不确定重跑。
    resolved: bool,
}

// 内部设计，不进入API docs：所有表/context均随Shared来源登记保活。
// 宿主持有来源注册的拥有权，所有C调用先经宿主enter，再解释本来源context。
// release_context仅在控制调用独占、全部条目清理成功后结束该拥有权。
// Shared真实布局不跨C读取。value在锁内只取得受控共享或移出；外部逻辑、
// 任意析构、Error/闭包的最后一次释放全部在锁外进行。
unsafe impl Send for ExportAddress {}
unsafe impl Sync for ExportAddress {}
// 来源登记保活、OnceLock单次发布、状态检查及控制调用独占共同保护该地址。
unsafe impl Send for SourceAddress {}
unsafe impl Sync for SourceAddress {}

impl Registry {
    /// 登记来源并持有代码。标准来源边界要求当前最终产物支持栈展开。
    // 本地步骤（local steps）：先建立稳定来源上下文及登记中状态，锁外登记；成功来源号只发布一次；失败保留上下文及清理归属，不发布伪成功。
    // @trace 24
    pub fn attach(connection: Connection) -> Result<Self> {
        inner::registry_attach(connection)
    }

    /// 来源内部导出真实Rust值；不修改原值的API，不跨库还原其布局。
    /// close返回Busy时保留值及进度；其它失败保留失败责任，成功后仅析构一次。
    // 本地步骤（local steps）：副作用前预留条目与恢复容量；准备对象及首持有；锁外预登记宿主释放义务；成功后发布并包装，失败载荷持续追踪。
    // @trace 25
    pub fn export<T: Send + Sync + 'static>(
        &self,
        value: T,
        interfaces: Vec<ExportedInterface<T>>,
        close: impl Fn(&T) -> Result<()> + Send + Sync + 'static,
    ) -> Result<Object> {
        inner::registry_export::<T>(self, value, interfaces, close)
    }

    /// 来源C方法在同一状态保护范围核验并取得本次访问，锁外调用body。
    /// T仅在本来源解释；同名Rust类型不作为跨构建兼容性证明。
    // 本地步骤（local steps）：同锁核来源、槽位、代际、轮次、持有、类型、接口及权限并增加active；锁外调用body；结束借用与临时Arc后归还active。
    // @trace 26
    pub fn with_access<T: Send + Sync + 'static, R>(
        &self,
        lease: LeaseV1,
        interface: InterfaceRequestV1,
        rights: u64,
        body: impl FnOnce(&T) -> Result<R>,
    ) -> Result<R> {
        inner::registry_with_access::<T, R>(self, lease, interface, rights, body)
    }

    /// 自定义来源C方法的共同入口；同步完成本次访问并在来源建立Outcome。
    ///
    /// # Safety
    /// context属于当前已取得代码保活的来源；lease及out使用遵守表合同。
    /// body仅通过本次T借用访问真实对象，不留存超出本次调用的引用。
    // 本地步骤（local steps）：最外层保护C处理；只解释本来源context；按with_access取得T并调用body；来源内部形成独立诊断或双分类结果。
    // @trace 27
    pub unsafe fn dispatch<T: Send + Sync + 'static>(
        context: *mut core::ffi::c_void,
        lease: LeaseV1,
        interface: InterfaceRequestV1,
        rights: u64,
        body: impl FnOnce(&T) -> Result<()>,
    ) -> OutcomeV1 {
        crate::boundary::catch_outcome(
            crate::boundary::Context {
                operation: "dispatch",
                module_epoch: lease.initialization.module_epoch,
                round: lease.initialization.round,
                source: lease.object.source,
            },
            || unsafe { inner::registry_dispatch::<T>(context, lease, interface, rights, body) },
        )
    }

    /// 撤销新业务；已经取得的访问继续保活，读取/清理按权限分别处理。
    // 本地步骤（local steps）：同锁把对象移入撤销阶段并禁止新业务；不打断已获准访问，读取及清理依权限继续；重放不回到Live。
    // @trace 28
    pub fn revoke(&self, key: ObjectKeyV1) -> Result<()> {
        inner::registry_revoke(self, key)
    }
    /// 有界推进已具备条件的清理，不撤销仍正常使用的对象。
    // 本地步骤（local steps）：每次只取得一个可推进条目的清理资格；锁外执行close；Busy原样归还，其它失败保留；成功后仅一次析构值及回调捕获。
    // @trace 29
    pub fn maintain(&self) -> Result<u64> {
        inner::registry_maintain(self)
    }
    /// 请求来源退役；Busy不等待，最终失败不重复已不确定的析构。
    // 本地步骤（local steps）：请求关闭并核所有持有、访问、诊断及清理；未齐Busy且不等待；不在最终独占检查中执行需重入本来源的业务清理。
    // @trace 30
    pub fn try_retire(&self) -> Result<()> {
        inner::registry_try_retire(self)
    }
}
impl Drop for Access {
    // 本地步骤（local steps）：先结束真实值借用并归还临时Arc，再在来源锁内扣一次active；不能使用户值的最后析构发生在锁内。
    // @trace 31
    fn drop(&mut self) {
        inner::access_drop(self)
    }
}
impl Drop for CleanupAttempt {
    // 本地步骤（local steps）：未resolved时把尚有归属的载荷与回调归回持续条目，记录未确认失败；不得盲目再次析构或丢失代码保活。
    // @trace 32
    fn drop(&mut self) {
        inner::cleanup_attempt_drop(self)
    }
}
impl Drop for Mutation {
    // 本地步骤（local steps）：未resolved时依据持久准备记录收拢本操作；已提交效果不撤回为从未发生，未知状态停止新业务并保留失败。
    // @trace 33
    fn drop(&mut self) {
        inner::mutation_drop(self)
    }
}
impl<T: Send + Sync + 'static> ExportedInterface<T> {
    /// 把本来源的静态C表关联到T。
    ///
    /// # Safety
    /// 表方法使用相应Registry核验T/持有/权限；表全生命周期不可变，
    /// 相关代码属于本登记来源。函数签名和准确尺寸符合I声明。
    // 本地步骤（local steps）：核I固定身份、主版本、准确表尺寸及共同权限掩码；只建立本来源T到只读C表的声明，不直接把T指针交出。
    // @trace 34
    pub unsafe fn new<I: AbiInterface>(table: &'static I::Table) -> Result<Self> {
        unsafe { inner::exported_interface_new::<T, I>(table) }
    }
}

// 下列接线属于本来源的唯一C分派出口；每一入口最外层独立捕获panic。
// context不得由任意地址构造，out仅在成功读取，全部调用先由宿主保持代码。
// 本地步骤（local steps）：检查合法来源及完整lease、接口和权限的当前状态；不增加持有或长期访问；返回成功不替代真正使用时核验。
// @trace 35
pub(crate) unsafe extern "C" fn validate(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
    request: InterfaceRequestV1,
    rights: u64,
) -> OutcomeV1 {
    crate::boundary::catch_outcome(
        crate::boundary::Context {
            operation: "validate",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::validate(context, lease, request, rights) },
    )
}
// 本地步骤（local steps）：副作用前建立同操作完整输入的恢复记录；准备新持有并预登记宿主义务后发布；重放活动结果，冲突或已归还结果不复活。
// @trace 36
pub(crate) unsafe extern "C" fn retain(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
    operation: OperationIdV1,
    out: *mut ObjectRefV1,
) -> OutcomeV1 {
    crate::boundary::catch_outcome(
        crate::boundary::Context {
            operation: "retain",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::retain(context, lease, operation, out) },
    )
}
// 本地步骤（local steps）：同操作匹配完整lease并把指定持有恰好归还一次；活动访问仍保留值；确认只表示来源接管后续清理，不等待回调。
// @trace 37
pub(crate) unsafe extern "C" fn release(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
    operation: OperationIdV1,
) -> OutcomeV1 {
    crate::boundary::catch_outcome(
        crate::boundary::Context {
            operation: "release",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::release(context, lease, operation) },
    )
}
// 本地步骤（local steps）：在保护内核lease和固定请求；只在成功写出借用context及表；管理表仅返回同轮受限管理能力，不泄露宿主内部能力。
// @trace 38
pub(crate) unsafe extern "C" fn query_interface(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
    request: InterfaceRequestV1,
    out: *mut InterfaceViewV1,
) -> OutcomeV1 {
    crate::boundary::catch_outcome(
        crate::boundary::Context {
            operation: "query_interface",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::query_interface(context, lease, request, out) },
    )
}
// 本地步骤（local steps）：核一次独立诊断持有；写出不可变分类及三段UTF-8字节数；失败不承诺输出值。
// @trace 39
pub(crate) unsafe extern "C" fn diagnostic_info(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
    out: *mut DiagnosticInfoV1,
) -> u32 {
    crate::boundary::catch_status(
        crate::boundary::Context {
            operation: "diagnostic_info",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::diagnostic_info(context, lease, out) },
    )
}
// 本地步骤（local steps）：在来源访问保护内核容量与指针前提；按user、verbose、debug顺序完整复制；成功written精确等于三段总长，空段不构造无效切片。
// @trace 40
pub(crate) unsafe extern "C" fn diagnostic_copy(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
    buffer: *mut u8,
    capacity: u64,
    written: *mut u64,
) -> u32 {
    crate::boundary::catch_status(
        crate::boundary::Context {
            operation: "diagnostic_copy",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::diagnostic_copy(context, lease, buffer, capacity, written) },
    )
}
// 先在锁外提交本来源已知的宿主释放记录，再确认持有归还。
// 这也允许初次管理查询失败时，在既有外层保护内显式归还自身诊断。
// 宿主已有Queued/Releasing记录时，重复提交不增加任务或递归执行release。
// 本地步骤（local steps）：在来源锁外先调用自身Connection的有限状态出口提交该诊断预登记记录；确认后同锁将指定持有恰好归还；宿主Queued或Releasing时提交幂等且不即时执行回调，不递归创建诊断。
// @trace 41
pub(crate) unsafe extern "C" fn diagnostic_release(
    context: *mut core::ffi::c_void,
    lease: LeaseV1,
) -> u32 {
    crate::boundary::catch_status(
        crate::boundary::Context {
            operation: "diagnostic_release",
            module_epoch: lease.initialization.module_epoch,
            round: lease.initialization.round,
            source: lease.object.source,
        },
        || unsafe { inner::diagnostic_release(context, lease) },
    )
}
// 本地步骤（local steps）：核来源号及同一退役操作；有界检查关闭条件；Busy保存进度；只在全部完成后确认可释放上下文。
// @trace 42
pub(crate) unsafe extern "C" fn try_retire(
    context: *mut core::ffi::c_void,
    source: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    crate::boundary::catch_outcome(
        crate::boundary::Context {
            operation: "try_retire",
            module_epoch: 0,
            round: 0,
            source: source,
        },
        || unsafe { inner::try_retire(context, source, operation) },
    )
}
// 本地步骤（local steps）：核来源号及同一推进操作；每次最多推进一个具备条件的条目并写remaining瞬时值；重放不重复执行已确认效果。
// @trace 43
pub(crate) unsafe extern "C" fn maintain(
    context: *mut core::ffi::c_void,
    source: u64,
    operation: OperationIdV1,
    out: *mut MaintenanceV1,
) -> OutcomeV1 {
    crate::boundary::catch_outcome(
        crate::boundary::Context {
            operation: "maintain",
            module_epoch: 0,
            round: 0,
            source: source,
        },
        || unsafe { inner::maintain(context, source, operation, out) },
    )
}
// 本地步骤（local steps）：仅在最终独占控制阶段接收已清空上下文的唯一拥有权；锁外释放；失败或结果不确定后不再解释该地址。
// @trace 44
pub(crate) unsafe extern "C" fn release_context(
    context: *mut core::ffi::c_void,
    source: u64,
) -> u32 {
    crate::boundary::catch_status(
        crate::boundary::Context {
            operation: "release_context",
            module_epoch: 0,
            round: 0,
            source: source,
        },
        || unsafe { inner::release_context(context, source) },
    )
}

impl Registry {
    /// 每次发布一份独立诊断，已有快照可用于重放。
    // @trace 45
    pub fn publish_management_failure(
        &self,
        failure: &crate::diagnostic::ManagementFailure,
    ) -> OutcomeV1 {
        inner::publish_management_failure(self, failure)
    }
}
