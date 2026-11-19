// 本地实现入口（local implementation entry）；关键类型由父模块声明。
use super::*;

pub(super) fn copied_diagnostic(error: &Error) -> Option<&CopiedDiagnostic> {
    todo!("输入（input）：error；本地步骤（local steps）：从本地pi_result附加数据查最近Arc<CopiedDiagnostic>并返回借用；没有则None，不重新格式化来源文本；输出（output）：签名约定的结果；边界（boundary）：状态由本来源或宿主分别拥有；验证（verification）：合法调用链覆盖正常、拒绝、重复、恐慌与清理失败。")
}

pub(super) fn snapshot(error: &Error) -> core::result::Result<Snapshot, SnapshotFailure> {
    todo!("输入（input）：error；本地步骤（local steps）：在来源边界读取公共分类并格式化三种文本；格式化恐慌或存储失败返回双分类，不发布半段诊断；输出（output）：签名约定的结果；边界（boundary）：状态由本来源或宿主分别拥有；验证（verification）：合法调用链覆盖正常、拒绝、重复、恐慌与清理失败。")
}

pub(super) fn copy_and_release(
    connection: &Connection,
    diagnostic: DiagnosticRefV1,
) -> Result<CopiedDiagnostic> {
    todo!("输入（input）：connection, diagnostic；本地步骤（local steps）：先保有一次独立诊断责任；在Connection调用保护内借用boundary::read_diagnostic复制；成功或失败都经有限状态submit_release交宿主，次级失败只在此可分配路径形成pi_result；输出（output）：签名约定的结果；边界（boundary）：状态由本来源或宿主分别拥有；验证（verification）：合法调用链覆盖正常、拒绝、重复、恐慌与清理失败。")
}

pub(super) fn decode(connection: &Connection, outcome: OutcomeV1) -> Result<()> {
    todo!("输入（input）：connection, outcome；本地步骤（local steps）：检查Outcome合法分支及分类一致性；有效失败保留原分类并附次级读取或释放失败；矛盾表示归Corrupted且保留原始编号事实；输出（output）：签名约定的结果；边界（boundary）：状态由本来源或宿主分别拥有；验证（verification）：合法调用链覆盖正常、拒绝、重复、恐慌与清理失败。")
}

pub(super) fn capture_management_failure(error: &Error) -> ManagementFailure {
    todo!("本来源提取公共分类和三种不可变文本，复用 snapshot，缺失时保存两项分类。")
}

pub(super) fn management_failure_without_source(failure: &ManagementFailure) -> OutcomeV1 {
    todo!("保留原操作分类，沿已知诊断失败分类，来源未建立时使用 InvalidState。")
}

pub(super) fn success_outcome() -> OutcomeV1 {
    todo!("结果零分类，诊断字段全零。")
}

pub(super) fn limited_outcome(operation: ErrorKind, diagnostic: ErrorKind) -> OutcomeV1 {
    todo!("按固定投影保留两项分类，诊断字段全零，不注册来源或分配。")
}

pub(super) fn status_code(kind: ErrorKind) -> u32 {
    todo!("经 boundary 的固定映射投影公共 ErrorKind。")
}

pub(super) fn protect_outcome(
    context: crate::boundary::Context,
    body: impl FnOnce() -> OutcomeV1,
) -> OutcomeV1 {
    todo!("复用 boundary::catch_outcome 包住整次操作，最外层仍失败时返回双分类。")
}

pub(super) fn protect_status(
    context: crate::boundary::Context,
    body: impl FnOnce() -> u32,
) -> u32 {
    todo!("复用 boundary::catch_status，不构造来源诊断。")
}

pub(super) unsafe fn decode_management(
    connection: Option<&Connection>,
    outcome: OutcomeV1,
) -> Result<()> {
    todo!("先核合法形状。独立诊断持有由调用方校验来源，复用 decode 复制并归还。无连接只解成功或有限失败。")
}
