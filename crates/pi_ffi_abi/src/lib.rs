#![no_std]
#![doc(html_no_source)]
//! 受控C表示（controlled C representation）及固定操作表。
//! 传输字段不拥有资源；复制字段不增加持有，不使原始地址自动有效。
//! 所有不安全调用都要求真实代码／上下文存续、正确布局及相应权限。

use core::ffi::c_void;

/// 固定16字节身份（UUID），逐字节比较，不依赖字符串格式。
#[repr(C)]
// @trace 1
pub struct UuidV1 {
    pub bytes: [u8; 16],
}
/// 所有操作表的固定首部（API header）。
#[repr(C)]
// @trace 2
pub struct ApiHeaderV1 {
    pub major: u32,
    pub size: u32,
}
/// 由宿主指定的初始化标识（initialization identifier）。
#[repr(C)]
// @trace 3
pub struct InitIdV1 {
    pub module_epoch: u64,
    pub round: u64,
}
/// 受控对象身份（object identity），旧代际不得复活。
#[repr(C)]
// @trace 4
pub struct ObjectKeyV1 {
    /// 已登记且进程内不复用的来源编号。
    pub source: u64,
    /// 来源内部对象槽位。
    pub slot: u64,
    /// 来源槽位代际，零值无效。
    pub generation: u64,
}
/// 独立来源持有（source holding）的完整传输身份。
#[repr(C)]
// @trace 5
pub struct LeaseV1 {
    /// 真实对象的完整来源身份。
    pub object: ObjectKeyV1,
    /// 对象所属初始化轮次。
    pub initialization: InitIdV1,
    /// 独立持有编号；复制不建立新持有。
    pub holding: u64,
}
/// 逻辑操作标识（operation identifier）；重试复用，另一次操作换号。
#[repr(C)]
// @trace 6
pub struct OperationIdV1 {
    pub actor: u64,
    pub sequence: u64,
}
/// 受控对象传输值（object transfer value）；必须另证代码与持有存续。
#[repr(C)]
// @trace 7
pub struct ObjectRefV1 {
    /// 只由来源代码解释的稳定上下文。
    pub context: *mut c_void,
    /// 固定来源基础表地址。
    pub api: *const SourceApiV1,
    /// 本次借用或独立移交的完整持有。
    pub lease: LeaseV1,
}
/// 准确接口请求（interface request），三项必须共同匹配。
#[repr(C)]
// @trace 8
pub struct InterfaceRequestV1 {
    /// 固定接口身份。
    pub interface_id: UuidV1,
    /// 接口协议主版本。
    pub major: u32,
    /// 接口表的准确字节数。
    pub size: u32,
}
/// 借用接口视图（borrowed interface view），不独立拥有代码或对象。
#[repr(C)]
// @trace 9
pub struct InterfaceViewV1 {
    /// 表调用必须使用的实际上下文。
    pub context: *mut c_void,
    /// 成功查询得到的固定只读表。
    pub table: *const c_void,
}
/// 一次独立交接的来源诊断（source diagnostic），必须配对归还。
#[repr(C)]
// @trace 10
pub struct DiagnosticRefV1 {
    /// 只由诊断来源解释的上下文。
    pub context: *mut c_void,
    /// 已登记诊断操作表。
    pub api: *const DiagnosticApiV1,
    /// 本次独立诊断持有；全零仅用于无诊断分支。
    pub lease: LeaseV1,
}
/// 不可变诊断信息（diagnostic information），长度单位为UTF-8字节。
#[repr(C)]
// @trace 11
pub struct DiagnosticInfoV1 {
    /// 固定pi_result公共分类编号。
    pub kind: u32,
    /// user段精确字节数，不含结尾零字节。
    pub user_len: u64,
    /// verbose段精确字节数。
    pub verbose_len: u64,
    /// debug段精确字节数。
    pub debug_len: u64,
}
/// 操作结果（operation outcome）：成功、独立诊断失败或双分类有限失败。
#[repr(C)]
// @trace 12
pub struct OutcomeV1 {
    /// 零为成功；非零为操作失败分类。
    pub kind: u32,
    /// 诊断不能建立时的次级分类；其它分支为零。
    pub diagnostic_failure_kind: u32,
    /// 仅独立诊断失败分支有效，其它分支必须全零。
    pub diagnostic: DiagnosticRefV1,
}
/// 固定来源操作表（source operation table）；每次使用分别核验访问资格。
#[repr(C)]
// @trace 13
pub struct SourceApiV1 {
    /// 准确主版本及表尺寸。
    pub header: ApiHeaderV1,
    /// 只作即时校验，不建立持有或长期访问。
    pub validate:
        Option<unsafe extern "C" fn(*mut c_void, LeaseV1, InterfaceRequestV1, u64) -> OutcomeV1>,
    /// 新建独立持有；同操作重放不重复创建。
    pub retain: Option<
        unsafe extern "C" fn(*mut c_void, LeaseV1, OperationIdV1, *mut ObjectRefV1) -> OutcomeV1,
    >,
    /// 归还指定持有，不承诺最终清理已完成。
    pub release: Option<unsafe extern "C" fn(*mut c_void, LeaseV1, OperationIdV1) -> OutcomeV1>,
    /// 按准确身份、主版本及尺寸查询借用表。
    pub query_interface: Option<
        unsafe extern "C" fn(
            *mut c_void,
            LeaseV1,
            InterfaceRequestV1,
            *mut InterfaceViewV1,
        ) -> OutcomeV1,
    >,
}
/// 固定诊断操作表（diagnostic operation table），辅助状态不递归分配诊断。
#[repr(C)]
// @trace 14
pub struct DiagnosticApiV1 {
    /// 准确主版本及表尺寸。
    pub header: ApiHeaderV1,
    /// 读取不可变分类和三段精确长度。
    pub info: Option<unsafe extern "C" fn(*mut c_void, LeaseV1, *mut DiagnosticInfoV1) -> u32>,
    /// 写入宿主缓冲区，成功时保证完整精确写入。
    pub copy: Option<unsafe extern "C" fn(*mut c_void, LeaseV1, *mut u8, u64, *mut u64) -> u32>,
    /// 恰好归还本次独立诊断持有，重放不重复销毁。
    pub release: Option<unsafe extern "C" fn(*mut c_void, LeaseV1) -> u32>,
}

/// 共同管理接口身份（host control interface identity）；本版候选选定后固定。
// @trace 15
pub const HOST_CONTROL_ID: UuidV1 = UuidV1 {
    bytes: [
        0xcc, 0x01, 0x1b, 0x16, 0x32, 0xb1, 0x4f, 0xae, 0xb7, 0x19, 0x96, 0x5f, 0x70, 0xfa, 0x1f,
        0x7f,
    ],
};
/// 共同管理协议主版本（host control protocol major）。
// @trace 16
pub const HOST_CONTROL_MAJOR: u32 = 1;
/// 来源控制协议主版本（source control protocol major）。
// @trace 17
pub const SOURCE_CONTROL_MAJOR: u32 = 1;

/// 表种类（table kind）：对象基础表。
// @trace 18
pub const TARGET_OBJECT: u32 = 1;
/// 表种类（table kind）：诊断基础表。
// @trace 19
pub const TARGET_DIAGNOSTIC: u32 = 2;

/// 本次调用目标（call target）；复制字段不建立独立持有。
#[repr(C)]
// @trace 20
pub struct CallTargetV1 {
    /// 来源侧上下文（source context），只由来源解释。
    pub context: *mut c_void,
    /// 已登记的基础表地址；宿主匹配登记记录后才允许读取。
    pub api: *const c_void,
    /// 完整持有（holding）；其所属来源必须已经登记。
    pub lease: LeaseV1,
    /// `TARGET_OBJECT`或`TARGET_DIAGNOSTIC`。
    pub kind: u32,
}

/// 调用保活标识（call token）；复制不增加活动调用。
#[repr(C)]
#[derive(Clone, Copy)]
// @trace 21
pub struct CallTokenV1 {
    /// 已登记且未复用的来源编号。
    pub source: u64,
    /// 来源内不复用的调用编号；零值无效。
    pub call: u64,
}

/// 一次有界清理推进后的瞬时状态（maintenance snapshot）。
#[repr(C)]
#[derive(Clone, Copy)]
// @trace 22
pub struct MaintenanceV1 {
    /// 尚未确认完成的来源清理条目数；不含仍正常使用的对象。
    /// 零只描述本次快照，不保证未来不再产生清理工作。
    pub remaining: u64,
}

/// 来源声明（source descriptor）；地址须受来源代码保活保护。
#[repr(C)]
#[derive(Clone, Copy)]
// @trace 23
pub struct SourceDescriptorV1 {
    /// 来源实际上下文；宿主不得还原其Rust类型。
    pub context: *mut c_void,
    /// 对象基础表；与诊断表至少一项有效。
    pub objects: *const SourceApiV1,
    /// 诊断基础表；与对象表至少一项有效。
    pub diagnostics: *const DiagnosticApiV1,
    /// 必需的来源控制表（source control table）。
    pub control: *const SourceControlApiV1,
}

/// 来源退役操作表（source retirement operation table）。
#[repr(C)]
// @trace 24
pub struct SourceControlApiV1 {
    /// 来源控制协议主版本与本表准确字节数。
    pub header: ApiHeaderV1,
    /// 有界推进已归还对象的清理，不关闭仍正常使用的来源或其它对象。
    pub maintain: Option<
        unsafe extern "C" fn(*mut c_void, u64, OperationIdV1, *mut MaintenanceV1) -> OutcomeV1,
    >,
    /// 有界检查并推进来源收尾；不等待活动调用。成功后才能释放上下文。
    pub try_retire: Option<unsafe extern "C" fn(*mut c_void, u64, OperationIdV1) -> OutcomeV1>,
    /// 来源收尾成功后恰好调用一次；失败或结果不确定时不得再访问上下文。
    pub release_context: Option<unsafe extern "C" fn(*mut c_void, u64) -> u32>,
}

/// 共同宿主管理操作表（host control operation table）。
///
/// 接口身份、主版本和准确尺寸共同匹配。所有必需函数都必须存在。
/// 每个实现边界捕获其承诺范围内的恐慌（panic），不跨C展开。
#[repr(C)]
// @trace 25
pub struct HostControlApiV1 {
    /// 共同管理协议主版本与本表准确字节数。
    pub header: ApiHeaderV1,
    /// 分配新逻辑操作的身份；业务重试必须复用已有身份。
    pub next_operation: Option<unsafe extern "C" fn(*mut c_void, *mut OperationIdV1) -> u32>,
    /// 登记来源及其代码保活；来源身份只由同一宿主管理权威分配。
    pub register_source: Option<
        unsafe extern "C" fn(*mut c_void, OperationIdV1, SourceDescriptorV1, *mut u64) -> OutcomeV1,
    >,
    /// 持有发布前登记唯一释放义务；重试不重复创建记录。
    pub track_holding: Option<unsafe extern "C" fn(*mut c_void, CallTargetV1) -> u32>,
    /// 解引用来源表前取得调用保活；权限仍须由来源在每次使用时核定。
    pub enter_call: Option<
        unsafe extern "C" fn(
            *mut c_void,
            CallTargetV1,
            OperationIdV1,
            u64,
            *mut CallTokenV1,
        ) -> u32,
    >,
    /// 恰好结束本次活动调用；重放不重复扣减，不在函数内卸载来源代码。
    pub leave_call: Option<unsafe extern "C" fn(*mut c_void, CallTokenV1) -> u32>,
    /// 无分配地提交既有释放义务；不执行来源回调，成功不等于最终清理成功。
    pub request_release: Option<unsafe extern "C" fn(*mut c_void, CallTargetV1) -> u32>,
    /// 推进指定来源退役；全部清理及上下文释放成功后才结束来源代码保活。
    pub retire_source: Option<unsafe extern "C" fn(*mut c_void, u64, OperationIdV1) -> OutcomeV1>,
}
