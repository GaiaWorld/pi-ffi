//! 可信构建与部署记录。

#[path = "package_inner.rs"]
mod inner;

use pi_result::Result;
use semver::{Version, VersionReq};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// 已批准的目标平台（target platform）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    LinuxX86_64Gnu,
    WindowsX86_64Msvc,
}

/// 构建清单的一项实际文件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildFile {
    /// 受信部署根以内的路径。
    pub path: PathBuf,
    /// 最终制品的 SHA-256。
    pub sha256: [u8; 32],
}

/// 装配使用的加载名与实际文件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildDependency {
    /// 二进制导入所用的 ELF 或 DLL 名称。
    pub load_name: String,
    /// 批准的唯一制品。
    pub file: BuildFile,
    /// 各阶段需要核对的实际用法。
    pub usage: DependencyUse,
}

/// 实际依赖（dependency）承担的角色。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyUse {
    Plugin,
    SharedRust,
    RuntimeInput,
}

/// 构建方确认的本插件静态声明（static declarations）的归属。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaticDeclarations {
    /// 该制品的构造器仅提交到本模块所拥有的表。
    PrivateInventory,
    /// 制品提供明确清单，构造器不会向共享表提交。
    ExplicitList,
    /// 制品没有静态条目或相关构造器。
    Empty,
}

/// 宿主实际使用的配套构建（matching build）声明。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostBuild {
    pub rust_build_sha256: [u8; 32],
    pub allocator: String,
}

/// 同一配套构建（matching build）提供的事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildManifest {
    pub target: Target,
    pub uuid: [u8; 16],
    pub version: Version,
    pub library: BuildFile,
    /// 完整部署需要核对的非系统依赖，包括实际交接共用的 Rust 制品。
    pub dependencies: Vec<BuildDependency>,
    /// 配套构建的工具链、目标、编译选项和实际依赖内容的摘要。
    pub rust_build_sha256: [u8; 32],
    /// Config 等原对象的分配与释放约定，由配套构建提供方建立。
    pub allocator: String,
    pub static_declarations: StaticDeclarations,
}

/// 宿主已确认可供本进程接纳的一个已部署插件包。
#[derive(Clone)]
pub struct PluginPackage {
    root: Arc<Path>,
    manifest: Arc<BuildManifest>,
}

/// 必须同时满足插件身份和版本要求。
#[derive(Clone, Debug)]
pub struct PluginRequest {
    pub uuid: [u8; 16],
    pub version: VersionReq,
}

/// 本库复制并校验的只读库元信息（read-only library metadata）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub uuid: [u8; 16],
    pub version: Version,
}

impl PluginPackage {
    /// 从宿主管理的可信部署记录建立候选插件包。
    ///
    /// # Safety
    ///
    /// 调用者已确认原生代码遵约，实际构建和分配器匹配，且受管制品、
    /// 共享依赖与进程加载条件在使用期间稳定。不得用任意文件填充此保证。
    /// 本库随后检查文件、映像及标准入口的实际身份。
    // @trace 1
    pub unsafe fn from_verified_build(root: PathBuf, manifest: BuildManifest) -> Result<Self> {
        unsafe { inner::package_from_verified_build(root, manifest) }
    }
}

impl PluginPackage {
    // @trace 2
    pub(crate) fn root(&self) -> &Path {
        inner::package_root(self)
    }

    // @trace 3
    pub(crate) fn manifest(&self) -> &BuildManifest {
        inner::package_manifest(self)
    }
}
