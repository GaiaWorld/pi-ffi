//! 本地错误与C边界保护（local error and C boundary protection）。

/* 内部设计，不进入API docs。
来源活动访问保留本地`Arc<Value>`，只在本来源把真实值解释为T。活动访问结束时先结束对T的借用、归还临时值持有，再减活动计数；来源登记仍拥有原值，不能在锁内意外触发最后析构。

每个有副作用的入口在执行可能恐慌的处理之前，建立持续可恢复的操作位置及作用域保护。恐慌发生后，已提交效果不回退成“从未执行”；未提交准备载荷不遗失，无法确认安全恢复的对象停止新业务并保留清理失败。不能只使用`AssertUnwindSafe`就声称业务状态恢复。

每个C入口最外层分别保护完整处理，包括输入检查、错误建立、文本格式化与释放。正常错误走`pi_result`，可捕获恐慌转为本地`pi_result`；最外层诊断建立也失败时返回约定的双分类分支。辅助整数状态入口返回有限分类，不递归分配来源错误。

恐慌负载（panic payload）只读取已知字符串类型，不调用任意格式化。其释放再次恐慌时，来源作用域仍须保留未知收尾及代码责任。显式终止、分配器终止与不可恢复二次恐慌按异常保证范围不保证恢复；不由宿主跨C捕获另一Rust产物的展开。
*/

#[path = "boundary_inner.rs"]
mod inner;

use pi_ffi_abi::{DiagnosticRefV1, ObjectRefV1, OutcomeV1};
use pi_result::{Error, ErrorKind, Result};
use std::sync::Arc;

/// 恐慌（panic）转换所需的、由当前调用方拥有的定位信息。
// @trace 1
pub struct Context {
    /// 固定操作名，不借用来源临时缓冲区。
    pub operation: &'static str,
    /// 已知模块编号；零只用于尚未绑定模块的本地操作。
    pub module_epoch: u64,
    /// 已知初始化轮次；预初始化阶段为零。
    pub round: u64,
    /// 已知来源编号；未知时为零。
    pub source: u64,
}

/// 在当前Rust构建允许栈展开（stack unwinding）时转换可捕获恐慌。
///
/// 调用者须先建立状态恢复及资源交接责任；捕获不证明业务状态可继续使用。
/// 显式终止、分配器终止和不可恢复二次恐慌不属于恢复保证。
// 本地步骤（local steps）：在本地产物捕获可展开恐慌；保留普通pi_result；只读取已知字符串负载，在独立保护内归还负载，无法确认时在本地报告附上 UnconfirmedPanicCleanup。
// @trace 2
pub fn catch<T>(context: Context, body: impl FnOnce() -> Result<T>) -> Result<T> {
    inner::catch::<T>(context, body)
}

// 本地 pi_result 报告携带的标记。捕获值的清理未确认时，由拥有来源的调用路径保留现有失败记录。
// @trace 16
#[derive(Debug)]
pub(crate) struct UnconfirmedPanicCleanup;

/// C边界（C boundary）的最外层保护；诊断建立再次失败时返回双分类分支。
///
// 内部设计，不进入API docs：任何有副作用的body在外部逻辑前准备恢复记录。
// catch成功不自动提交该记录；body完成或其恢复路径明确提交。
// panic payload只识别本地String及静态字符串，不调用任意Display。
// payload释放也在来源边界内；无法确认释放时保留所属来源与失败责任。
// 本地步骤（local steps）：保护完整C入口；普通返回原样交出；恐慌或诊断再次失败时构造操作分类与诊断建立分类，不递归分配诊断。
// @trace 3
pub(crate) fn catch_outcome(context: Context, body: impl FnOnce() -> OutcomeV1) -> OutcomeV1 {
    inner::catch_outcome(context, body)
}

/// 有限状态辅助入口的最外层保护，不递归创建诊断。
// 本地步骤（local steps）：保护有限整数状态入口；正常状态保留，可捕获恐慌映射本地pi_result分类编号，不创建来源诊断。
// @trace 4
pub(crate) fn catch_status(context: Context, body: impl FnOnce() -> u32) -> u32 {
    inner::catch_status(context, body)
}
/// 宿主自有复制结果；不包含来源Report、trait object或来源分配器对象。
#[derive(Debug)]
// @trace 5
pub struct CopiedDiagnostic {
    /// 接收侧公共pi_result分类。
    pub kind: ErrorKind,
    /// 宿主自有user文本，不借用来源内存。
    pub user: String,
    /// 宿主自有verbose文本。
    pub verbose: String,
    /// 宿主自有debug文本。
    pub debug: String,
    /// 未认识的非零C分类；公开分类映射为Other。
    pub raw_kind: u32,
}

/// 同一本地报告内共享文本；不引用来源模块、来源函数或来源Report。
#[derive(Debug)]
// @trace 6
pub(crate) struct CopiedContext {
    /// 与附加数据共享本地文本，不保留来源代码。
    pub(crate) diagnostic: Arc<CopiedDiagnostic>,
}

impl core::fmt::Display for CopiedDiagnostic {
    // 本地步骤（local steps）：仅按固定标签读取本地三段文本，不访问来源地址，不把本地重新格式化结果冒充来源原文。
    // @trace 7
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        inner::copied_diagnostic_fmt(self, f)
    }
}
impl core::fmt::Display for CopiedContext {
    // 本地步骤（local steps）：只写本地共享快照的user文本，格式器错误原样返回。
    // @trace 8
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        inner::copied_context_fmt(self, f)
    }
}
impl std::error::Error for CopiedContext {}

impl CopiedDiagnostic {
    /// 形成仅依赖本地数据的pi_result错误，三种来源诊断均保留。
    /// 公共ErrorKind继续为当前上下文；报告本身的格式化不承诺逐字等于来源报告。
    // 本地步骤（local steps）：把快照置于本地Arc；以CopiedContext建立报告并保留当前ErrorKind，附加共享快照，不安装全局格式化钩子。
    // @trace 9
    pub fn into_error(self) -> Error {
        inner::copied_diagnostic_into_error(self)
    }
}

// 本地步骤（local steps）：按固定17类映射显式编码ErrorKind；不使用Rust枚举内存表示，不以未定义的编号表示成功。
// @trace 10
pub(crate) fn encode_kind(kind: ErrorKind) -> u32 {
    inner::encode_kind(kind)
}
// 本地步骤（local steps）：按固定映射解释分类编号；未知非零值归Other；原始编号由调用者另行保留，零值只由合法成功分支接受。
// @trace 11
pub(crate) fn decode_kind(code: u32) -> ErrorKind {
    inner::decode_kind(code)
}

// 诊断表实际报告类别与本次结果原类别不符。作为 pi_result 诊断的附加数据，
// 区分协议矛盾与一次合法诊断的读取或释放失败。报告可打印两个原始类别。
// @trace 14
#[derive(Debug)]
pub(crate) struct DiagnosticKindMismatch {
    pub(crate) expected: u32,
    pub(crate) reported: u32,
}

impl core::fmt::Display for DiagnosticKindMismatch {
    // @trace 15
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        inner::diagnostic_kind_mismatch_fmt(self, f)
    }
}

/// 读取已受外层代码与来源持有保护的不可变诊断；本函数不接管归还责任。
///
/// # Safety
/// diagnostic的上下文、表和独立持有在整个调用中有效；调用方仍负责唯一归还。
/// 表和对象访问必须已由管理调用或合法引导外层保护，不能仅以非空地址证明。
// 本地步骤（local steps）：只借用已保护诊断；先比对结果的原 C 分类与诊断分类，再核三段长度、总和范围及完整精确复制并校验UTF-8；不结束传入持有，归还由调用方负责。
// @trace 12
pub(crate) unsafe fn read_diagnostic(
    diagnostic: &DiagnosticRefV1,
    expected_kind: u32,
) -> Result<CopiedDiagnostic> {
    unsafe { inner::read_diagnostic(diagnostic, expected_kind) }
}

/// 首次管理查询及输入retain的诊断交接；不依赖尚未建立的Connection。
///
/// # Safety
/// 外层保护覆盖seed来源上下文及其自身诊断表，持续到本函数返回。
/// outcome唯一移交，且标准来源的错误诊断由该来源自身创建；不同来源返回拒绝读取。
/// 来源诊断release必须先把该诊断的既有释放记录提交给来源已持有的宿主连接。
// 先核 Outcome 合法分支和诊断的来源、上下文及轮次（round）。仅在合法独立失败
// 分支传入其非零类别，在原有保护内调用固定读取入口。保护结束前显式归还；保留类别
// 矛盾的 Corrupted，其它读取或归还失败依原错误报告方式处理。
// @trace 13
pub(crate) unsafe fn decode_bootstrap(seed: &ObjectRefV1, outcome: OutcomeV1) -> Result<()> {
    unsafe { inner::decode_bootstrap(seed, outcome, read_diagnostic) }
}
