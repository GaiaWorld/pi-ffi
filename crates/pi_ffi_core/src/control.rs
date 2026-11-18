//! 共同宿主管理连接（shared host control connection）。

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

`Connection::from_borrowed`是唯一的外来输入引导入口。调用方在外层已经持有该输入的代码和上下文，才允许查询管理表。先查询、核固定身份／准确尺寸及全部必需方法，再经共同权威取新操作标识、为输入取得独立持有。此持有本身也已由来源在发布前预登记宿主释放义务。

标准初始化接口（standard initialization API）先核两个必传输入。配置（Config）的
外层调用保护（call protection）有效时，从它建立连接（connection）。
用现有连接分别为终端（terminal）和有值的运行时（runtime）取得本次代码保活，
查询共同管理接口并核对管理上下文、表及初始化标识。
两项必传输入和有值的运行时通过核验后才进插件初始化。没有运行时输入时跳过该项。
失败时把已取得的输入持有（holding）提交给宿主继续释放。

宿主本地连接分别保存内部登记能力与返回给插件的受限能力，不能把宿主代码归属的能力透传。`from_native`只用于同一本地Rust边界，传入的存续持有必须确实保护两个管理视图；其中原生特征对象不跨C传递。该持有不得暗中承载业务收尾，最终归还也不得直接卸载仍在返回的代码。

首次查询管理表可能返回独立错误诊断，尚不能依赖管理连接。为闭合这一分支，首次管理查询按以下方式处理：

- `boundary`拥有不依赖管理连接的诊断字节读取原语、接收侧自有文本类型及显式分类投影；`diagnostic`保留来源快照、普通受管理交接与公开读取出口，重新导出接收类型。五模块编译依赖保持单向。
- `boundary::decode_bootstrap`仅处理seed同来源、同上下文、同初始化作用域的独立诊断。标准来源的query／retain错误必须在自身来源创建诊断，不能直接转交另一来源的原始Outcome；转发错误先在本地转为pi_result再形成自身诊断。
- 引导外层保护覆盖该来源代码、上下文及其诊断表。读取／复制完整包在本地catch中；之后仍在该保护内显式调用来源诊断release。释放不放入Drop，不依赖尚未取得的Connection，不在失败时改调上层模块。
- 来源诊断release先通过来源自己已经拥有的连接，提交该诊断预登记的宿主释放记录，再确认来源持有归还。宿主重复提交已有Queued／Releasing记录不加任务、不即时调用来源；后续宿主重放来源release仍幂等。提交失败保持宿主责任及代码，有限状态返回并形成接收侧次级失败。
- 不匹配来源的结果判Corrupted，不读取或释放未经保护的地址；宿主保留既有登记与版本占用，后续来源退役会看到未清责任。此为拒绝失约数据，不声称已回收其任意私有资源。

连接克隆共享同一绑定，不创建备用权威。最后连接结束时，仅对管理表所依附的输入持有提交既有释放记录；管理表必须至少有效到提交函数返回。
*/

#[path = "control_inner.rs"]
mod inner;

use parking_lot::Mutex;
use pi_ffi_abi::{
    CallTargetV1, CallTokenV1, HostControlApiV1, InitIdV1, ObjectRefV1, OperationIdV1, OutcomeV1,
    SourceDescriptorV1,
};
use pi_result::Result;
use std::sync::Arc;

/// 同一宿主管理上下文（host control context）的本地共享连接。
/// 克隆只共享连接，不创建另一份进程管理权威。
// @trace 1
pub struct Connection {
    pub(crate) binding: Arc<Binding>,
}

// 内部设计，不进入API docs：来源代码所有者与初始化作用域分别由宿主绑定。
// Binding拥有一份管理接口所依附的输入持有；管理表借用该holding的存续。
// 所有其它holding对外发布前都已登记宿主release record。
// Binding最后释放仅调用无分配的request_release；不直接执行来源release。
// @trace 2
pub(crate) struct Binding {
    /// 本地登记所用管理地址；由protection保持存续。
    pub(crate) management: ManagementAddress,
    /// 查询时交给接收方的受限上下文；宿主自身登记能力不得直接泄露给插件。
    pub(crate) forwarded_management: ManagementAddress,
    /// 本连接唯一初始化作用域，不能以字段复制建立新轮。
    pub(crate) initialization: InitIdV1,
    /// 管理代码及上下文的唯一保活来源。
    pub(crate) protection: Protection,
    /// 仅本地连接阶段的互斥保护。
    pub(crate) state: Mutex<BindingState>,
}
// @trace 3
pub(crate) enum Protection {
    /// 经来源独立持有保护管理视图。
    Input(InputHolding),
    /// 仅在同一本地Rust边界内传递，不把此trait object作为C表示。
    Native(Arc<dyn Send + Sync>),
}
/// 仅由已核来源、独立持有及持久释放记录共同建立的输入保活。
// @trace 4
pub(crate) struct InputHolding {
    target: CallTargetV1,
}

// 安全前提：构造仅在有效外层保护中完成；来源表/context遵守跨线程合同；
// 独立holding及宿主持续记录保留代码和上下文；后续使用仍先取得调用资格；
// 末次归还只提交既有记录。此证明不适用于任意CallTargetV1或原生Rust指针。
unsafe impl Send for InputHolding {}
unsafe impl Sync for InputHolding {}

/// 已证明受该输入持有保护的管理表地址；不接受任意指针构造。
// @trace 5
pub(crate) struct ManagementAddress {
    /// 管理方拥有的受控上下文，仅管理表解释。
    pub(crate) context: *mut core::ffi::c_void,
    /// 固定只读管理操作表地址，不独立取得保活。
    pub(crate) table: *const HostControlApiV1,
}

// @trace 6
pub(crate) enum BindingState {
    /// 管理视图可按每次调用合同使用。
    Live,
    /// 管理输入持有已提交宿主释放。
    ReleaseSubmitted,
    /// 归还不确定，禁止继续使用相关地址。
    Quarantined,
}

/// 一次受保护的来源调用（protected source call）。
/// 最后归还失败则宿主保留调用与代码，不强行减少计数。
// @trace 7
pub(crate) struct Call {
    /// 调用期间保留同一管理连接。
    pub(crate) binding: Arc<Binding>,
    /// 宿主登记的唯一活动调用标识。
    pub(crate) token: CallTokenV1,
    /// 只有归还确认后为true，避免重复扣减。
    pub(crate) ended: bool,
}

impl Connection {
    /// 宿主为同一初始化作用域建立本地连接；普通插件不得自行调用。
    ///
    /// # Safety
    /// lifetime确实持有两个管理视图的代码和上下文；management只承接宿主自身登记，
    /// forwarded只授予目标插件作用域的能力；两者来自同一实际权威。
    /// 此Rust入口不跨动态库传递原生trait object。
    // 本地步骤（local steps）：在外部存续保护内核两个管理视图及作用域；分别保存宿主内部能力与外发受限能力；失败不取得原生业务对象的销毁责任。
    // @trace 8
    pub unsafe fn from_native(
        management: pi_ffi_abi::InterfaceViewV1,
        forwarded: pi_ffi_abi::InterfaceViewV1,
        initialization: InitIdV1,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<Self> {
        unsafe { inner::connection_from_native(management, forwarded, initialization, lifetime) }
    }

    /// 从初始化的借用输入建立共享连接。普通业务接口不调用此入口。
    /// 查询或retain失败时，在同一外层保护内经boundary::decode_bootstrap归还自身诊断。
    ///
    /// # Safety
    /// 外层调用已持有seed的来源代码与上下文；seed是合法可借用持有，
    /// 所有必需C表真实有效。成功后本连接取得独立该输入持有。
    // 本地步骤（local steps）：在外层保护内查询管理表，错误交boundary::decode_bootstrap；成功核固定表并取操作标识，retain输入仍在原借用保护内处理自身诊断；独立持有确认后建立绑定，后续失败只作有限状态提交。
    // @trace 9
    pub unsafe fn from_borrowed(seed: ObjectRefV1) -> Result<Self> {
        unsafe { inner::connection_from_borrowed(seed) }
    }

    /// 核对另一项输入属于同一管理上下文、表和初始化轮次。
    ///
    /// # Safety
    /// peer来自当前受保护初始化调用的合法输入，禁止伪造任意地址。
    /// 外层代码保护也覆盖peer自身诊断；错误交接不调用上层diagnostic模块。
    // 本地步骤（local steps）：先经本连接取得peer代码持有再查询；来源错误在同一保护内经boundary::decode_bootstrap处理；比较实际管理表、上下文及初始化标识，不调用上层diagnostic模块。
    // @trace 10
    pub unsafe fn check_peer(&self, peer: ObjectRefV1) -> Result<()> {
        unsafe { inner::connection_check_peer(self, peer) }
    }

    /// 当前绑定的初始化标识（initialization identifier）。
    // 本地步骤（local steps）：逐字段重建绑定的初始化标识，只返回数据快照，不建立新轮次或持有。
    // @trace 11
    pub fn initialization(&self) -> InitIdV1 {
        inner::connection_initialization(self)
    }

    // 本地步骤（local steps）：通过管理表分配全新逻辑操作编号；零值、耗尽和失败转换为pi_result，不回绕或复用编号。
    // @trace 12
    pub(crate) fn next_operation(&self) -> Result<OperationIdV1> {
        inner::connection_next_operation(self)
    }
    // 本地步骤（local steps）：先核本地连接状态再调用已保护的管理表；仅成功时取得调用标识并构造唯一Call；失败不制造活动资格。
    // @trace 13
    pub(crate) fn enter(
        &self,
        target: CallTargetV1,
        operation: OperationIdV1,
        rights: u64,
    ) -> Result<Call> {
        inner::connection_enter(self, target, operation, rights)
    }
    // 本地步骤（local steps）：在持有对外发布前登记宿主释放责任；核完整来源及持有，重复确认不创建第二份责任。
    // @trace 14
    pub(crate) fn track(&self, target: CallTargetV1) -> Result<()> {
        inner::connection_track(self, target)
    }
    /// 无分配提交既有释放记录；零成功，非零为pi_result固定分类投影。
    /// 此出口不创建Report，最终失败由宿主持续记录；显式Rust操作按需形成报告。
    // 本地步骤（local steps）：无分配向宿主提交预登记持有，只返回既有pi_result整数投影；不创建Report、不执行来源回调；失败由宿主持续保留责任及代码。
    // @trace 15
    pub(crate) fn submit_release(&self, target: CallTargetV1) -> u32 {
        inner::connection_submit_release(self, target)
    }

    // 原始Outcome只能由上层诊断封装在有效代码保护内解码，不对普通用户公开。
    // 本地步骤（local steps）：经受保护管理表提交来源声明及同一操作标识；只在成功时写出来源号；原始Outcome留给上层诊断出口处理。
    // @trace 16
    pub(crate) fn register_raw(
        &self,
        operation: OperationIdV1,
        descriptor: SourceDescriptorV1,
        out: &mut u64,
    ) -> OutcomeV1 {
        inner::connection_register_raw(self, operation, descriptor, out)
    }
    // 本地步骤（local steps）：向同一权威提交来源退役请求；保留Busy及原始失败结果，不直接读取退役后的来源地址。
    // @trace 17
    pub(crate) fn retire_raw(&self, source: u64, operation: OperationIdV1) -> OutcomeV1 {
        inner::connection_retire_raw(self, source, operation)
    }
}

impl Clone for Connection {
    // 本地步骤（local steps）：仅克隆本地Arc绑定，共享已有独立持有，不执行Cretain或分配新操作标识。
    // @trace 18
    fn clone(&self) -> Self {
        inner::connection_clone(self)
    }
}
impl Drop for Binding {
    // 本地步骤（local steps）：在锁外对Input保护提交既有释放记录；Native只结束本地代码存续持有；不执行业务收尾或卸载正在返回的代码。
    // @trace 19
    fn drop(&mut self) {
        inner::binding_drop(self)
    }
}
impl Call {
    // 本地步骤（local steps）：恰好提交一次leave_call；确认后标ended；失败保留宿主记录并以pi_result返回，不重复扣减。
    // @trace 20
    pub(crate) fn finish(mut self) -> Result<()> {
        inner::call_finish(self)
    }
}
impl Drop for Call {
    // 本地步骤（local steps）：尚未结束时归还既有调用标识；失败由宿主持续记录；不分配新责任，不直接卸载来源。
    // @trace 21
    fn drop(&mut self) {
        inner::call_drop(self)
    }
}

// 内部设计，不进入API docs：ManagementAddress具有受控地址的人工Send/Sync证明。
// 证明前提：受控构造、只读固定表、上下文线程安全、管理接口所依附的输入持有及宿主call记录保活；
// unsafe impl不得推广到任意裸指针或原生Rust对象。Mutex内只改变本地管理状态。
unsafe impl Send for ManagementAddress {}
unsafe impl Sync for ManagementAddress {}
