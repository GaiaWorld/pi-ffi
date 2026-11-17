//! 共享对象拥有者与类型化调用（shared object owner and typed call）。

/* 内部设计，不进入API docs。
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

`Object::interface`先取得代码保活，再执行来源查询，核接口身份、主版本、准确尺寸与支持的共同权限。成功只建立依附本地拥有者的借用表；缓存表地址不获得永久访问许可。

`Interface::with_call`先取得本次调用保活，才向闭包提供表及本次目标。`InterfaceCall`内部借用连接，不可由外部结构体字面量任意构造。适配器通过其`decode`把本次C结果转为`pi_result`，通过其连接把新返回的受控持有包装为本地拥有者；无须访问核心私有模块。

调用顺序为：进入宿主调用→来源表调用→在本次保护范围核返回并复制诊断→提交诊断释放义务→归还调用。实际能力方法在来源自己的状态保护范围再次核持有、类型、轮次、权限及阶段并登记活动访问；预校验成功不替代这次核定。

闭包不能返回借自本次表访问的Rust引用。复制原始地址也不延长任何生命周期（lifecycle）；之后解引用仍需要相应不安全前提。适配器须在收到成功的独立持有后，立即建立拥有权恢复包装，再做可能失败的加工。
*/

#[path = "object_inner.rs"]
mod inner;

use crate::control::Connection;
use parking_lot::Mutex;
use pi_ffi_abi::{InitIdV1, InterfaceRequestV1, LeaseV1, ObjectRefV1, OutcomeV1, UuidV1};
use pi_result::Result;
use std::marker::PhantomData;
use std::sync::Arc;

/// 受控C接口（controlled C interface）的固定声明。
///
/// # Safety
/// Table须采用本协议允许的C表示、首字段为ApiHeaderV1、所有调用签名固定；
/// UUID/major在不兼容变化时不得复用。表方法只通过受保护来源访问真实对象。
// @trace 1
pub unsafe trait AbiInterface: 'static {
    /// 本接口固定C表的真实类型；不含原生Rust对象表示。
    type Table: Send + Sync + 'static;
    /// 一次选定并固定的接口身份。
    const ID: UuidV1;
    /// 不兼容变化必须更换的协议主版本。
    const MAJOR: u32;
    /// 该接口接受的共同权限位并集；只允许已定义的1／2／4。
    const SUPPORTED_RIGHTS: u64;
}

/// 一个来源持有（source holding）的本地共享拥有者。
// @trace 2
pub struct Object {
    /// 本地别名共同拥有的一份来源持有。
    pub(crate) owner: Arc<ObjectOwner>,
}
// @trace 3
pub(crate) struct ObjectOwner {
    /// 持有提交释放所需的同轮管理连接。
    pub(crate) connection: Connection,
    /// 唯一独立持有的传输字段，仅来源解释。
    pub(crate) raw: ObjectRefV1,
    /// 本地持有提交进度，不能证明来源清理完成。
    pub(crate) state: Mutex<OwnerState>,
}
// @trace 4
pub(crate) enum OwnerState {
    /// 本地拥有者尚可发起受保护调用。
    Live,
    /// 独立持有已提交，不能再次使用。
    ReleaseSubmitted,
    /// 交接不确定，宿主继续保留责任。
    Quarantined,
}

/// 不可克隆的独立持有移交包装（holding transfer wrapper）。
/// 未交出时销毁，也归还其已登记的释放义务。
// @trace 5
pub struct TransferObject {
    /// 未交出前负责提交释放的同轮连接。
    pub(crate) connection: Connection,
    /// 只允许接管一次的独立来源持有。
    pub(crate) raw: ObjectRefV1,
    /// 已交出时本地析构不得重复提交释放。
    pub(crate) transferred: bool,
}

/// 依附对象持有的已识别接口（identified interface）。
// @trace 6
pub struct Interface<I: AbiInterface> {
    /// 接口视图依附的本地共享持有。
    pub(crate) object: Object,
    /// 成功查询得到的表及其实际上下文。
    pub(crate) address: TableAddress<I>,
}

/// 仅在本次调用期间可读取的操作表（operation table）与目标信息。
// @trace 7
pub struct InterfaceCall<'call, I: AbiInterface> {
    /// 只在本次代码保护范围内借用的C操作表。
    pub table: &'call I::Table,
    /// 查询实际返回的上下文，调用时原样交回。
    pub context: *mut core::ffi::c_void,
    /// 当前持有字段；复制不增加持有。
    lease: LeaseV1,
    /// 与本次代码保护同寿命的本地连接借用。
    connection: &'call Connection,
}

// 内部设计，不进入API docs：TableAddress不是任意非空指针的安全包装。
// 只能在Object合法holding与Call保活下，从成功query的已核对表构造；
// 后续每次访问都重新enter，不能把query的验证当成永久访问许可。
// @trace 8
pub(crate) struct TableAddress<I: AbiInterface> {
    /// 保存query_interface实际返回的上下文，不能改用基础对象上下文。
    pub(crate) context: *mut core::ffi::c_void,
    /// 已核固定身份及尺寸的只读表地址。
    pub(crate) pointer: *const I::Table,
    /// 关联接口类型，不拥有该类型的Rust值。
    pub(crate) marker: PhantomData<I>,
}

impl Object {
    /// 接管一份已在宿主登记的独立持有。
    ///
    /// # Safety
    /// raw拥有权确实移交本调用，未被其它拥有者接管；原始表示和所属连接合法。
    /// 复制相同字段后重复调用不满足此前提。失败时收尾归属不得丢失。
    // 本地步骤（local steps）：先为已唯一移交的持有建立恢复责任，再核连接及来源；本地包装失败也提交既有释放记录，不泄漏或重复接管。
    // @trace 9
    pub unsafe fn from_transfer(connection: Connection, raw: ObjectRefV1) -> Result<Self> {
        unsafe { inner::object_from_transfer(connection, raw) }
    }
    /// 本对象所属轮次的只读数据副本；不访问来源代码。
    // 本地步骤（local steps）：逐字段重建本对象所属初始化标识；不取得新持有，不查询已退役来源。
    // @trace 10
    pub fn initialization(&self) -> InitIdV1 {
        inner::object_initialization(self)
    }
    /// 新建独立来源持有；操作失败不改变当前拥有者。
    // 本地步骤（local steps）：保留当前拥有者并取得调用资格；新操作retain后立刻接管返回的独立持有，再做失败加工；当前对象不因失败失效。
    // @trace 11
    pub fn try_retain(&self) -> Result<Self> {
        inner::object_try_retain(self)
    }
    /// 新建独立持有供另侧恰好接管一次；当前拥有者保持有效。
    // 本地步骤（local steps）：按新独立retain取得一次可移交持有并建立不可Clone包装；未交出前任何失败都提交该持有。
    // @trace 12
    pub fn retain_for_transfer(&self) -> Result<TransferObject> {
        inner::object_retain_for_transfer(self)
    }
    /// 结束当前本地别名；最后一个别名提交既有释放责任，不承诺最终清理完成。
    // 本地步骤（local steps）：以Arc::try_unwrap区分最后本地拥有者；有别名时仅结束当前别名；最后拥有者经ObjectOwner::submit_release提交，显式失败在返回前另建本地pi_result；Drop不重复提交。
    // @trace 13
    pub fn release(self) -> Result<()> {
        inner::object_release(self)
    }
    /// 查询固定接口，成功后共享当前本地持有；不延长已撤销操作权限。
    // 本地步骤（local steps）：在本次代码保护内执行query；核固定身份、主版本、准确尺寸、权限及非空地址；保存实际返回context并共享本地拥有者。
    // @trace 14
    pub fn interface<I: AbiInterface>(&self) -> Result<Interface<I>> {
        inner::object_interface::<I>(self)
    }
}
impl Clone for Object {
    // 本地步骤（local steps）：只克隆本地共享拥有者，不跨C创建独立持有，不改变来源计数。
    // @trace 15
    fn clone(&self) -> Self {
        inner::object_clone(self)
    }
}
impl ObjectOwner {
    /// 同一持有至多提交一次；只返回有限分类，不构造Report或调用来源代码。
    // 本地步骤（local steps）：锁内核唯一提交状态并领取提交资格；锁外调用Connection::submit_release的有限状态出口，保存已提交或隔离状态；不创建Report或临时释放记录。
    // @trace 24
    pub(crate) fn submit_release(&self) -> u32 {
        inner::object_owner_submit_release(self)
    }
}
impl Drop for ObjectOwner {
    // 本地步骤（local steps）：仅经ObjectOwner::submit_release有限状态出口提交；不构造Report、不分配记录、不调用来源回调；失败由宿主持续可观察，不能在Drop内改走显式release。
    // @trace 16
    fn drop(&mut self) {
        inner::object_owner_drop(self)
    }
}

impl TransferObject {
    /// 消费唯一移交包装；接收者必须遵守from_transfer的接管及失败清理要求。
    /// 获得字段不允许在未持有来源保活时直接解引用或调用其中地址。
    // 本地步骤（local steps）：消费唯一移交包装并标记transferred；逐字段交出原始表示；不增加持有，不直接访问来源对象。
    // @trace 17
    pub fn into_raw(self) -> ObjectRefV1 {
        inner::transfer_object_into_raw(self)
    }
}
impl Drop for TransferObject {
    // 本地步骤（local steps）：未交出的独立持有只提交预登记责任；已交出时不重复提交；失败继续可观察。
    // @trace 18
    fn drop(&mut self) {
        inner::transfer_object_drop(self)
    }
}

impl<I: AbiInterface> Interface<I> {
    /// 先取得本次代码保活，再借用表；来源方法仍须取得本次对象访问资格。
    /// 不允许返回借自call的引用；闭包用InterfaceCall::decode处理C结果。
    // 本地步骤（local steps）：核共同权限后enter；用实际查询context、当前lease和借用连接构造调用参数；执行闭包后归还调用，失败合并且不覆盖原错误。
    // @trace 19
    pub fn with_call<R>(
        &self,
        rights: u64,
        body: impl for<'call> FnOnce(InterfaceCall<'call, I>) -> Result<R>,
    ) -> Result<R> {
        inner::interface_with_call::<I, R>(self, rights, body)
    }
}
impl<'call, I: AbiInterface> InterfaceCall<'call, I> {
    /// 重建本次持有的C传输值；不移动调用框架，也不建立另一份持有。
    // 本地步骤（local steps）：逐字段重建当前lease传输值，不移动调用参数或建立独立持有。
    // @trace 20
    pub fn lease(&self) -> LeaseV1 {
        inner::interface_call_lease::<I>(self)
    }

    /// 供类型化适配器在当前调用中接管新返回的受控持有。
    // 本地步骤（local steps）：借用同一本地连接供本次返回值接管；不泄露管理裸地址或新建权威。
    // @trace 21
    pub fn connection(&self) -> &'call Connection {
        inner::interface_call_connection::<I>(self)
    }

    /// 将本次C返回结果转为pi_result，并复制／提交释放其独立诊断。
    ///
    /// # Safety
    /// outcome来自当前受保护且遵约的调用；其中独立诊断恰好移交一次，
    /// 不得把按位复制的同一返回结果重复接管。
    // 本地步骤（local steps）：在尚有效的本次代码保护内把唯一Outcome交诊断出口处理；所有合法诊断均提交归还，不允许同一结果二次接管。
    // @trace 22
    pub unsafe fn decode(&self, outcome: OutcomeV1) -> Result<()> {
        unsafe { inner::interface_call_decode::<I>(self, outcome) }
    }
}
impl<I: AbiInterface> Clone for Interface<I> {
    // 本地步骤（local steps）：共享对象本地拥有者并重建已核表地址；不解引用来源表，不创建新访问资格。
    // @trace 23
    fn clone(&self) -> Self {
        inner::interface_clone::<I>(self)
    }
}
unsafe impl<I: AbiInterface> Send for TableAddress<I> {}
unsafe impl<I: AbiInterface> Sync for TableAddress<I> {}

// ObjectRefV1含裸指针，不直接对传输类型声明Send/Sync。
// ObjectOwner及TransferObject的受控构造、来源线程合同、Connection和已登记holding
// 联合证明跨线程访问/最终释放；不允许以这些impl证明来源T自动线程安全。
unsafe impl Send for ObjectOwner {}
unsafe impl Sync for ObjectOwner {}
unsafe impl Send for TransferObject {}
unsafe impl Sync for TransferObject {}
