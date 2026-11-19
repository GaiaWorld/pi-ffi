//! 宿主自有诊断数据（host-owned diagnostic data）。

/* 内部设计，不进入API docs。
普通来源错误先提取公共分类和user／verbose／debug三种不可变UTF-8文本；完整诊断发布前也建立宿主释放记录。原生`Report`及其附件在来源边界内处理，不作为可重放结果长期埋入正常去重缓存。

正常失败缓存保存不可变文本快照；每次交接创建可独立归还的诊断持有。诊断无法建立时只缓存操作分类与诊断失败分类。不能缓存一次性原始诊断句柄并把它重复当成新的拥有权交出。不可恢复清理失败可以留存来源错误，但它必须阻止最终退役，不能藏在“清理已完成”的上下文中。

宿主复制严格按以下顺序：

1. 核`OutcomeV1`三种合法分支；混合或矛盾表示为协议损坏（Corrupted）。
2. 在代码保护下读取诊断信息，核分类一致、长度总和及u64到usize／isize范围。
3. 由宿主预留完整缓冲区，来源完整写入；仅成功读取，字节数须精确一致。
4. 按三段长度校验UTF-8，构造宿主自有字符串；零长度不对空指针构造Rust切片。
5. 无论读取／复制能否完成，都提交已经合法接管的诊断释放义务；次级失败加入宿主自有报告，不吞掉。

有效来源操作失败保留其公共分类；诊断建立、读取、复制或提交释放的次级失败附独立阶段／分类及已取得数据。若返回表示自身矛盾，当前公共分类用Corrupted，并把来源声称的原始分类作为原始事实保存，不能当成已证明的业务原因。未知非零编号映射Other且保存原值。

接收侧`CopiedDiagnostic`只含本地错误分类、原始编号和三个自有字符串。`CopiedContext`与报告附加数据共享同一个本地快照，不复制大文本，也不保存来源函数指针或Report。公共报告仍为`pi_result::Error`，当前上下文是公共ErrorKind。需要原始三种文本时调用`copied_diagnostic`读取附加数据；本地报告重新格式化会包含本地上下文，不承诺和来源格式逐字相等。不安装全局格式化钩子（formatting hook）。
*/

#[path = "diagnostic_inner.rs"]
mod inner;

pub use crate::boundary::CopiedDiagnostic;
use crate::control::Connection;
use pi_ffi_abi::{DiagnosticRefV1, OutcomeV1};
use pi_result::{Error, ErrorKind, Result};
use std::sync::Arc;

/// 来源自有且建立后不可变的诊断快照（diagnostic snapshot）。
// @trace 1
pub(crate) struct Snapshot {
    /// 来源操作的公共pi_result分类。
    pub(crate) kind: ErrorKind,
    /// user格式的完整UTF-8文本。
    pub(crate) user: String,
    /// verbose格式的完整UTF-8文本。
    pub(crate) verbose: String,
    /// debug格式的完整UTF-8文本。
    pub(crate) debug: String,
}

/// 建立诊断失败时仍可交接的两项错误分类。
// @trace 2
pub(crate) struct SnapshotFailure {
    /// 原操作错误分类，诊断失败不得覆盖。
    pub(crate) operation: ErrorKind,
    /// 诊断建立阶段的独立失败分类。
    pub(crate) diagnostic: ErrorKind,
    /// 格式化时捕获的本地值仍未确认清理。
    pub(crate) cleanup_unconfirmed: bool,
}

/// 可重放的来源失败；原生Report的处理在来源边界完成后才提交此记录。
// @trace 5
pub(crate) enum CachedFailure {
    /// 共享不可变三文本快照，不保存外来Rust报告。
    Complete(Arc<Snapshot>),
    /// 诊断不能建立时保留两项公共分类。
    Unavailable {
        operation: ErrorKind,
        diagnostic: ErrorKind,
    },
}

/// 读取最近一次导入的原始诊断文本；没有该附加数据时返回None。
/// 三种文本按来源复制值读取，不能用本地报告重新格式化代替。
// 本地步骤（local steps）：从本地pi_result附加数据查最近Arc<CopiedDiagnostic>并返回借用；没有则None，不重新格式化来源文本。
// @trace 9
pub fn copied_diagnostic(error: &Error) -> Option<&CopiedDiagnostic> {
    inner::copied_diagnostic(error)
}

// 本地步骤（local steps）：在来源边界读取公共分类并格式化三种文本；格式化恐慌或存储失败返回双分类，不发布半段诊断。
// @trace 10
pub(crate) fn snapshot(error: &Error) -> core::result::Result<Snapshot, SnapshotFailure> {
    inner::snapshot(error)
}
// 本地步骤（local steps）：先保有一次独立诊断责任；在Connection调用保护内以原 Outcome 分类调用 boundary::read_diagnostic，先核类别再复制；成功或失败都经有限状态submit_release交宿主，次级失败只在此可分配路径形成pi_result。
// @trace 11
pub(crate) fn copy_and_release(
    connection: &Connection,
    diagnostic: DiagnosticRefV1,
    expected_kind: u32,
) -> Result<CopiedDiagnostic> {
    inner::copy_and_release(connection, diagnostic, expected_kind)
}
// 仅将已核合法独立失败的非零 Outcome.kind 转给复制入口。类别矛盾保留
// Corrupted 及可打印的两原值。普通读取或释放失败作为原操作错误的次级诊断。
// @trace 12
pub(crate) fn decode(connection: &Connection, outcome: OutcomeV1) -> Result<()> {
    inner::decode(connection, outcome)
}

/// 可重复发送的管理错误（management error）快照。
/// 每次发送都新建独立诊断持有。
pub struct ManagementFailure {
    pub(crate) cached: CachedFailure,
    /// 原错误或诊断格式化过程含尚未确认的清理。
    pub(crate) cleanup_unconfirmed: bool,
    pub(crate) report_cleanup: Option<ReportCleanup>,
}

/// 原错误与实际清理错误（cleanup error）的分类。
pub(crate) struct ReportCleanup {
    pub(crate) original_kind: ErrorKind,
    pub(crate) cleanup_kind: ErrorKind,
}

impl ManagementFailure {
    /// 在本地提取 pi_result 分类和三段诊断。
    // 借用原报告。同步保留原报告或格式化过程传入的清理标记，由拥有来源的路径记录失败。
    // @trace 13
    pub fn capture(error: &Error) -> Self {
        inner::capture_management_failure(error)
    }

    /// 根据已知清理结果完成本地管理错误（management error）。
    ///
    /// 原报告正常销毁时给 None。销毁未确认时给已捕获的分类和本地诊断文本。
    /// 返回更新后的快照与清理是否仍未确认。保留已经取得的原诊断。
    /// 原 Busy 因清理未确认需要结束本次尝试时，返回 Internal 并保存原分类说明。
    // @trace 21
    pub fn finish_report_cleanup(
        self,
        disposal: Option<(ErrorKind, String)>,
    ) -> (Self, bool) {
        inner::management_failure_finish_report_cleanup(self, disposal)
    }

    /// 没有可用来源时投影快照的操作及诊断建立分类。
    /// 合并清理结果后使用已经确定的最终分类。
    // @trace 14
    pub fn without_source(&self) -> OutcomeV1 {
        inner::management_failure_without_source(self)
    }
}

/// 形成没有诊断持有的成功结果。
/* 共同结果的零字段由本模块统一构造。 */
// @trace 15
pub fn success_outcome() -> OutcomeV1 {
    inner::success_outcome()
}

/// 两项分类的有限失败。用在无法建立独立诊断的情况。
// @trace 16
pub fn limited_outcome(operation: ErrorKind, diagnostic: ErrorKind) -> OutcomeV1 {
    inner::limited_outcome(operation, diagnostic)
}

/// 把 pi_result 公共分类投影到受控 C 状态码。
// @trace 17
pub fn status_code(kind: ErrorKind) -> u32 {
    inner::status_code(kind)
}

/// 保护完整的标准 C 管理结果，包括诊断失败及可展开恐慌。
// @trace 18
pub fn protect_outcome(
    context: crate::boundary::Context,
    body: impl FnOnce() -> OutcomeV1,
) -> OutcomeV1 {
    inner::protect_outcome(context, body)
}

/// 保护仅返回有限状态的共同 C 操作。
// @trace 19
pub fn protect_status(context: crate::boundary::Context, body: impl FnOnce() -> u32) -> u32 {
    inner::protect_status(context, body)
}

/// 从受保护的标准管理调用取得宿主自有 pi_result。
///
/// # Safety
///
/// 调用方持有本次标准入口的实际模块，独立诊断仅移交一次。
/// 有独立诊断时，连接应为本轮，先验证来源登记及模块归属，再调用来源表。
/// 无轮次或来源时只能接受成功或有限失败，不解引用返回指针。
// @trace 20
pub unsafe fn decode_management(
    connection: Option<&Connection>,
    outcome: OutcomeV1,
) -> Result<()> {
    unsafe { inner::decode_management(connection, outcome) }
}
