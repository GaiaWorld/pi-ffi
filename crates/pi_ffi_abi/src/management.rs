//! 插件的三类同步管理接口（management exports）。

pub use crate::{InitIdV1, ObjectRefV1, OutcomeV1, UuidV1};
use core::ffi::CStr;

/// 只读库元信息（read-only library metadata）的精确导出名。
pub const METADATA_V1: &CStr = c"pi_ffi_metadata_v1";
/// 三输入初始化（initialization）的精确导出名。
pub const INITIALIZE_V1: &CStr = c"pi_ffi_initialize_v1";
/// 四输入初始化（initialization）的精确导出名。实际允许能力由 pi_ffi 的特性（features）决定。
pub const INITIALIZE_V2: &CStr = c"pi_ffi_initialize_v2";
/// 关闭（close）的精确导出名。
pub const CLOSE_V1: &CStr = c"pi_ffi_close_v1";

/// 模块内声明的只读元信息（read-only metadata）。
/// 宿主持有模块时复制版本字节，插件所有权保持原状。
#[repr(C)]
pub struct MetadataV1 {
    /// 跨版本稳定身份（UUID），全零无效。
    pub uuid: UuidV1,
    /// 模块内不可变 UTF-8 字节，无终止符要求。
    pub version: *const u8,
    /// 完整语义化版本（SemVer）的字节数。
    pub version_len: u64,
}

/// 本次请求明确尚无需插件关闭。
pub const INIT_NOT_STARTED: u32 = 1;
/// 本轮已完成插件初始化。
pub const INIT_COMPLETED: u32 = 2;
/// 本轮可能已有插件侧效果，需要以本轮标识关闭。
pub const INIT_NEEDS_CLOSE: u32 = 3;

/// 插件已知进度独立于诊断建立和读出。
#[repr(C)]
pub struct InitReplyV1 {
    /// 本协议三项固定进度之一。成功仅与 INIT_COMPLETED 配对。
    pub progress: u32,
    /// B1 共同结果。成功以外的进度须与失败配对。
    pub outcome: OutcomeV1,
}

/// 每次只写宿主提供的输出槽。成功时输出完整有效。
pub type MetadataFnV1 = unsafe extern "C" fn(*mut MetadataV1) -> OutcomeV1;
/// 三个输入按运行时、终端、配置排列。运行时可为空。
pub type InitializeFnV1 =
    unsafe extern "C" fn(*const ObjectRefV1, *const ObjectRefV1, *const ObjectRefV1) -> InitReplyV1;
/// 四个输入按运行时、终端、配置、初始化数据（initialization data）排列。
/// 运行时可为空；其余必传，包括数据类型为 `()` 的情况。
/// 每项借自同一次受保护的同步调用，不能用复制字段代替真实持有。
pub type InitializeFnV2 = unsafe extern "C" fn(
    *const ObjectRefV1,
    *const ObjectRefV1,
    *const ObjectRefV1,
    *const ObjectRefV1,
) -> InitReplyV1;
/// 按初始化标识关闭，旧轮不能影响新轮。
pub type CloseFnV1 = unsafe extern "C" fn(InitIdV1) -> OutcomeV1;

/// 标准管理接口（management interface）的三类职责，与具体版本分开。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementInterface {
    /// 初始化（initialization）：按选定规范接收本轮真实输入。
    Initialization,
    /// 只读元信息（read-only metadata）：读取稳定 UUID 和完整语义化版本（SemVer）。
    Metadata,
    /// 关闭（close）：按原初始化标识结束插件本轮工作并处理其清理责任。
    Close,
}

/// 接口规范版本（interface version）的目录状态；不表示编译调用能力。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceVersionStatus {
    /// 可用（Available）：允许新声明和接入；不表示本次已编入对应能力，调用仍须核对实际导出。
    Available,
    /// 已撤回（Yanked）：禁止新声明和调用，完整目录及历史说明仍保留。
    Yanked,
}

/// 完整历史目录的一条只读记录，不是允许调用的凭证。
pub struct InterfaceVersion {
    /// 所属的管理接口（management interface）类别；版本号只在该类别内解释。
    pub interface: ManagementInterface,
    /// 本类别的协议主版本号（protocol major version），对应准确名称、签名和语义。
    pub major: u32,
    /// 该规范在制品（artifact）中的准确 C 导出名（export name）。
    pub symbol: &'static CStr,
    /// 对应特性（feature）的名字；撤回后仍保留历史名字。
    pub feature: &'static str,
    /// 是否为本类别的最新版；同一类别恰有一个未撤回的最新版。
    pub latest: bool,
    /// 本规范的目录状态（catalog status）；可用不表示本次已编入对应调用能力。
    pub status: InterfaceVersionStatus,
    /// 同一类别的相邻前版主版本号（previous major version）；无前版时为 None，不跨类别关联。
    pub previous: Option<u32>,
    /// 相对同类别前版的变化摘要（change summary）；首版描述初始语义。
    pub change: &'static str,
}

/// 目录内容或状态变化时递增；过程宏（procedural macro）核对目标库配套。
pub const INTERFACE_VERSION_CATALOG_REVISION: u32 = 1;

/// 所有曾实现版本；不随 pi_ffi 的特性（features）过滤，当前无撤回项。
pub const INTERFACE_VERSIONS: &[InterfaceVersion] = &[
    InterfaceVersion {
        interface: ManagementInterface::Initialization,
        major: 1,
        symbol: INITIALIZE_V1,
        feature: "init_v1",
        latest: false,
        status: InterfaceVersionStatus::Available,
        previous: None,
        change: "首版：运行时、终端、配置三个指针；没有初始化数据 D。",
    },
    InterfaceVersion {
        interface: ManagementInterface::Initialization,
        major: 2,
        symbol: INITIALIZE_V2,
        feature: "init_v2",
        latest: true,
        status: InterfaceVersionStatus::Available,
        previous: Some(1),
        change: "相对 v1 增加第四指针，按值交付 D，并保留唯一消费及失败清理。",
    },
    InterfaceVersion {
        interface: ManagementInterface::Metadata,
        major: 1,
        symbol: METADATA_V1,
        feature: "metadata_v1",
        latest: true,
        status: InterfaceVersionStatus::Available,
        previous: None,
        change: "首版：写出只读 UUID 及版本；无前版。",
    },
    InterfaceVersion {
        interface: ManagementInterface::Close,
        major: 1,
        symbol: CLOSE_V1,
        feature: "close_v1",
        latest: true,
        status: InterfaceVersionStatus::Available,
        previous: None,
        change: "首版：按初始化标识关闭；无前版。",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 完整目录（catalog）保留四项准确规范，每类恰有一个可用最新版。
    #[test]
    fn complete_catalog_has_one_available_latest_per_interface() {
        assert_eq!(INTERFACE_VERSIONS.len(), 4);
        for interface in [ManagementInterface::Initialization, ManagementInterface::Metadata, ManagementInterface::Close] {
            assert_eq!(INTERFACE_VERSIONS.iter().filter(|entry| entry.interface == interface && entry.latest && entry.status == InterfaceVersionStatus::Available).count(), 1);
        }
        let mut initialization = INTERFACE_VERSIONS.iter().filter(|entry| entry.interface == ManagementInterface::Initialization);
        let first = initialization.next().unwrap();
        let second = initialization.next().unwrap();
        assert_eq!(first.symbol, INITIALIZE_V1);
        assert_eq!(first.feature, "init_v1");
        assert_eq!(first.previous, None);
        assert!(!first.latest);
        assert_eq!(second.symbol, INITIALIZE_V2);
        assert_eq!(second.previous, Some(1));
        assert!(INTERFACE_VERSIONS.iter().all(|entry| entry.status == InterfaceVersionStatus::Available));
    }
}
