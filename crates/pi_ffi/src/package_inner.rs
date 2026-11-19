use super::*;

pub(super) unsafe fn package_from_verified_build(
    root: PathBuf,
    manifest: BuildManifest,
) -> Result<PluginPackage> {
    todo!("确认目标、身份、完整版本、规范化根路径、路径限制和清单一致性。保留明确的构建与部署前提，尚不系统加载。")
}

pub(super) fn package_root(package: &PluginPackage) -> &Path {
    todo!("借用已知部署根，不访问外部代码。")
}

pub(super) fn package_manifest(package: &PluginPackage) -> &BuildManifest {
    todo!("借用已确认清单。")
}
