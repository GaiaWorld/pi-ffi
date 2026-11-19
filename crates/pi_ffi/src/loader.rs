//! 平台加载及实际模块核对（module verification）。

#[path = "loader_inner.rs"]
mod inner;

use crate::package::{BuildManifest, HostBuild, Metadata, PluginPackage, UnloadPolicy};
use pi_ffi_abi::management::{CloseFnV1, InitializeFnV1, MetadataFnV1, MetadataV1};
use pi_result::Result;
use std::{fs::File, mem::ManuallyDrop, path::PathBuf, sync::Arc};

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
use libloading::os::unix::Library as SystemLibrary;

#[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
use libloading::os::windows::Library as SystemLibrary;

/// 磁盘文件的真实身份，和模块地址分开。
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum FileIdentity {
    Unix {
        device: u64,
        inode: u64,
    },
    Windows {
        volume_serial: u64,
        file_id: [u8; 16],
    },
}

pub(crate) struct VerifiedFile {
    path: PathBuf,
    identity: FileIdentity,
    held_file: File,
}

pub(crate) struct VerifiedFiles {
    package: PluginPackage,
    main: VerifiedFile,
    dependencies: Vec<VerifiedFile>,
}

// 已核主文件和配套文件的稳定资料。进程级保留记录只存数据和系统引用。
// @trace 12
pub(crate) struct VerifiedDeployment {
    // 沿实际核验的清单、目标、版本和已绑定的卸载选择。
    package: PluginPackage,
    // 原始打开文件在目标平台的真实身份。
    main: FileIdentity,
    // 与清单中的配套文件顺序对应。对照时按目标平台的加载名匹配。
    dependencies: Vec<FileIdentity>,
}

/// 当本库持续拥有本次系统持有时才可使用的实际模块身份。
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct MappedIdentity {
    address: usize,
}

/// 受进程记录保护时才能调用的同一模块入口。
#[derive(Clone, Copy)]
pub(crate) struct Entrypoints {
    pub(crate) metadata: MetadataFnV1,
    pub(crate) initialize: InitializeFnV1,
    pub(crate) close: CloseFnV1,
}

pub(crate) struct VerifiedImage {
    pub(crate) identity: MappedIdentity,
    pub(crate) entries: Entrypoints,
    files: Arc<VerifiedFiles>,
    dependencies: Vec<MappedDependency>,
}

struct MappedDependency {
    identity: MappedIdentity,
    file: FileIdentity,
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
type NativeHandle = *mut core::ffi::c_void;

#[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
type NativeHandle = isize;

/// 系统实际交付的一份引用。可以在进程记录持有，或一次显式归还。
pub(crate) struct NativeModule {
    system: ManuallyDrop<SystemLibrary>,
    // 与 system 同一本次装入。只有持有有效时才借用，不单独释放。
    handle: NativeHandle,
}

// 安全前提：Linux 句柄（handle）是本条唯一系统引用内的系统值。
// 移动记录不解引用该值，仍由相同的 libloading 引用持有并显式归还。
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
unsafe impl Send for NativeModule {}

/*
Linux 调用 dlopen 的 NOW | LOCAL，dlinfo 取得实际 link_map 并核实际路径／文件，
dlsym 地址经 dladdr1 比对归属。校验区间基于所持模块的可读加载段。Windows
用全路径 LoadLibraryExW 及单次调用 DLL_LOAD_DIR | SYSTEM32。按路径、打开
文件的 FILE_ID 及实际 HMODULE 核对，GetModuleHandleExW 按地址确认入口归属。
地址归属核对使用 FROM_ADDRESS | UNCHANGED_REFCOUNT；对返回的模块句柄只比较
归属。局部将 libloading 的 isize 句柄按同一位值交给 windows-sys 的 HMODULE。
用 VirtualQuery 核元信息可读区间。被借用的地址不脱离模块保护。

装入前核读原文件、摘要、实际 ELF／PE x86_64 DLL／共享对象、三个直接导出及
依赖。构建及部署前提覆盖平台运行的构造器和 inventory 私有表。配套 Rust 核验
使用宿主构建记录及实际可见的相同依赖制品。程序映射表不替代导入绑定证明。
系统装入成功后经 libloading 的 into_raw/from_raw 无失败移回唯一引用，随引用
保留实际句柄（handle）。不再次装入，不新增系统引用；移动回后才向上层交付。
核对阶段只借用此句柄。移出 NativeModule 后只显式归还一次。禁止依赖默认
Drop 归还或吞掉系统错误；系统释放开始后不再借用原句柄。
*/

// @trace 1
pub(crate) fn file_identity(package: &PluginPackage) -> Result<FileIdentity> {
    inner::file_identity(package)
}

// @trace 2
pub(crate) fn verify_files(
    package: &PluginPackage,
    host: &HostBuild,
) -> Result<Arc<VerifiedFiles>> {
    inner::verify_files(package, host)
}

// @trace 3
pub(crate) fn open(files: &VerifiedFiles) -> Result<NativeModule> {
    inner::open(files)
}

// @trace 4
pub(crate) fn verify_image(
    module: &NativeModule,
    files: Arc<VerifiedFiles>,
) -> Result<VerifiedImage> {
    inner::verify_image(module, files)
}

// @trace 5
pub(crate) fn check_inputs(image: &VerifiedImage, has_runtime: bool) -> Result<()> {
    inner::check_inputs(image, has_runtime)
}

// @trace 6
pub(crate) unsafe fn copy_metadata(image: &VerifiedImage, raw: MetadataV1) -> Result<Metadata> {
    unsafe { inner::copy_metadata(image, raw) }
}

// @trace 7
pub(crate) fn close(module: NativeModule) -> Result<()> {
    inner::close(module)
}

impl VerifiedFiles {
    // @trace 9
    pub(crate) fn identity(&self) -> FileIdentity {
        inner::verified_file_identity(self)
    }

    // @trace 10
    pub(crate) fn manifest(&self) -> &BuildManifest {
        inner::verified_manifest(self)
    }

    /// 对照两份已核资料的主文件、配套依赖、构建和静态声明归属。
    /// 名称按目标平台比较。路径别名和声明顺序不改变实际文件身份。
    /// 只读取已有资料。插件身份与版本由宿主从元信息核对。
    // @trace 11
    pub(crate) fn matches_reuse(&self, candidate: &Self) -> bool {
        inner::verified_files_match_reuse(self, candidate)
    }

    // 把已打开文件的实际身份及必要可信资料复制到不含文件句柄（file handle）的记录。
    // @trace 13
    pub(crate) fn deployment(&self) -> Result<Arc<VerifiedDeployment>> {
        inner::verified_files_deployment(self)
    }

    // @trace 14
    pub(crate) fn unload_policy(&self) -> UnloadPolicy {
        inner::verified_files_unload_policy(self)
    }
}

impl VerifiedDeployment {
    // @trace 15
    pub(crate) fn identity(&self) -> FileIdentity {
        inner::verified_deployment_identity(self)
    }

    // 按已核构建、完整插件身份、实际文件、依赖的用途和卸载选择逐项对照。
    // @trace 16
    pub(crate) fn matches_reuse(&self, candidate: &VerifiedFiles) -> bool {
        inner::verified_deployment_match_reuse(self, candidate)
    }
}

impl VerifiedImage {
    // 核对实际映像和管理记录共同关联的是同一份文件持有。
    // @trace 17
    pub(crate) fn shares_files(&self, files: &Arc<VerifiedFiles>) -> bool {
        inner::verified_image_shares_files(self, files)
    }
}

/// 在有效系统引用内取得实际映像标识，尚不调用任何插件入口。
// @trace 8
pub(crate) fn mapped_identity(module: &NativeModule) -> Result<MappedIdentity> {
    inner::mapped_identity(module)
}
