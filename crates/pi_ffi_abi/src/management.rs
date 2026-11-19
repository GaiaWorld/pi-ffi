//! 插件的三项同步管理导出（management exports）。

pub use crate::{InitIdV1, ObjectRefV1, OutcomeV1, UuidV1};
use core::ffi::CStr;

/// 只读库元信息（read-only library metadata）的精确导出名。
pub const METADATA_V1: &CStr = c"pi_ffi_metadata_v1";
/// 初始化（initialization）的精确导出名。
pub const INITIALIZE_V1: &CStr = c"pi_ffi_initialize_v1";
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
/// 按初始化标识关闭，旧轮不能影响新轮。
pub type CloseFnV1 = unsafe extern "C" fn(InitIdV1) -> OutcomeV1;
