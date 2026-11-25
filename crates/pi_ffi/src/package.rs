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

// 装入（load）和同一实际制品的复用均核对这一选择；不可在持有映像期间更改。
// @trace 6
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum UnloadPolicy {
    // 退役管理代（management generation）后仍持有实际引用至进程退出。
    RetainUntilExit,
    // 完成本轮并退出本库的实际持有后允许显式归还。
    ReleaseWhenQuiescent,
}

/// 宿主已确认可供本进程接纳的一个已部署插件包。
#[derive(Clone)]
pub struct PluginPackage {
    root: Arc<Path>,
    manifest: Arc<BuildManifest>,
    unload_policy: UnloadPolicy,
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
    /// `root` 是可信部署根，`manifest` 指向实际构建及其配套文件。返回的包
    /// 使用默认卸载规则。成功装入后，退役管理代时保留本库的实际系统引用
    /// （system reference），供操作系统在进程退出时执行剩余收尾。
    /// 此入口检查部署路径，可能访问文件系统，调用本身不执行插件代码。
    /// 已装入且被拒的制品依其已知引用与核验结果处理。
    ///
    /// # Safety
    ///
    /// 调用者已确认原生代码遵约，实际构建和分配器匹配，且受管制品、
    /// 共享依赖与进程加载条件在使用期间稳定。不得用任意文件填充此保证。
    /// 通过类型预核后实际接收的初始化数据（initialization data）D 使用双方约定的同一实际定义，
    /// 其目标、工具链、特性、依赖、内存布局（layout）和分配器（allocator）配套；
    /// 间接析构和回调代码（drop/callback code）持续有效至最后使用与清理。
    /// `Send + Sync + 'static` 与类型身份相等不能替代上述构建保证。
    /// 实际登记类型与接收类型不同属于可拒绝的输入（input），不是要求调用者事先保证的类型相等。
    /// 加载失败且系统未交付引用时，构造和失败收尾不得留下随后需执行的插件代码。
    /// 本库随后检查文件、映像及标准入口的实际身份。
    // @trace 1
    pub unsafe fn from_verified_build(root: PathBuf, manifest: BuildManifest) -> Result<Self> {
        unsafe { inner::package_from_verified_build(root, manifest) }
    }

    /// 为有实际验证依据的配套构建（matching build）选择提前卸载（unloading）。
    ///
    /// 参数、部署路径检查和返回包同 [`Self::from_verified_build`]。取得此包不装入
    /// 动态库或发布服务。已结束插件工作且本库归还相关持有后，
    /// [`crate::host::Plugin::unload`] 可以显式归还本库拥有的系统引用。
    /// 操作系统仍可能因其它持有者而保留映像。
    ///
    /// # Safety
    ///
    /// 满足 [`Self::from_verified_build`] 的全部前提。调用者还须验证目标平台、
    /// 实际制品、配套依赖和该包在安全接口（safe API）下的全部使用，包括所有克隆、
    /// 后续轮次的输入、业务拒绝、部分失败、重试和关闭。制品须在产生影响卸载的效果
    /// 前拒绝超出验证范围的输入，或证明这些输入的失败收尾同样安全。
    /// 本库归还引用前，所有仍需使用映像的工作、回调、任务和线程局部存储（TLS）
    /// 收尾均须退出。系统终止回调须可安全完成，之后不得留下仍需本映像的任务。
    /// 上述保证随实际构建绑定，并须在包和由其取得的所有管理入口的使用期间成立。
    // @trace 4
    pub unsafe fn from_verified_unloadable_build(
        root: PathBuf,
        manifest: BuildManifest,
    ) -> Result<Self> {
        unsafe { inner::package_from_verified_unloadable_build(root, manifest) }
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

    // @trace 5
    pub(crate) fn unload_policy(&self) -> UnloadPolicy {
        inner::package_unload_policy(self)
    }
}
