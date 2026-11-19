use super::*;

pub(super) fn file_identity(package: &PluginPackage) -> Result<FileIdentity> {
    todo!("按实际打开的文件取得系统文件 ID。只用于候选比较，不装入。")
}

pub(super) fn verify_files(
    package: &PluginPackage,
    host: &HostBuild,
) -> Result<Arc<VerifiedFiles>> {
    todo!("比对宿主配套构建和分配器，核关键依赖的装入条件。对受管路径、目标、直接导出和摘要做装入前核验。持有文件到最终可归还。")
}

pub(super) fn open(files: &VerifiedFiles) -> Result<NativeModule> {
    todo!("有已登记恢复位置才用平台单次参数装入。获取一个真实系统引用，保存已检查的装入条件识别错误。")
}

pub(super) fn verify_image(
    module: &NativeModule,
    files: Arc<VerifiedFiles>,
) -> Result<VerifiedImage> {
    todo!("在真实持有内确认实际主映像、必需的实际配套依赖及三个确切入口来源。拒绝未知实际归属和可见冲突。")
}

pub(super) fn check_inputs(image: &VerifiedImage, has_runtime: bool) -> Result<()> {
    todo!("标准初始化前重核所需 Rust 库实际映射及分配者。Config 和终端的条件始终核，运行时的原生交接条件按实际有无核。")
}

pub(super) unsafe fn copy_metadata(image: &VerifiedImage, raw: MetadataV1) -> Result<Metadata> {
    todo!("只在实际模块调用保护内核地址、可读区间和长度，读取 UTF-8，解析完整 SemVer。宿主持有最终字符串及完整值。")
}

pub(super) fn close(module: NativeModule) -> Result<()> {
    todo!("已禁止取得新调用资格且原持有均退出后，消费一次真实系统引用。报告系统结果，不推导物理卸载；不在析构中重试。")
}

pub(super) fn verified_file_identity(files: &VerifiedFiles) -> FileIdentity {
    todo!("复制已核对的实际主文件身份，外层仍持有该文件。")
}

pub(super) fn mapped_identity(module: &NativeModule) -> Result<MappedIdentity> {
    todo!(
        "Linux 取得本次活 link_map，Windows 取得本次 HMODULE。系统持有仍在，不解引用任何标准入口。"
    )
}

pub(super) fn verified_manifest(files: &VerifiedFiles) -> &BuildManifest {
    todo!("借用与当前已核文件共同保存的完整构建清单，支持同映像复用及拒绝记录。")
}
