use super::*;
use crate::package::{BuildFile, Target};
use object::{Architecture, BinaryFormat, ExportTarget, NameOrOrdinal, Object, ObjectKind};
use pi_ffi_abi::management::{CLOSE_V1, INITIALIZE_V1, METADATA_V1};
use pi_result::{attachment::PathAttachment, Error, ErrorKind};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

pub(super) fn file_identity(package: &PluginPackage) -> Result<FileIdentity> {
    let file = open_verified_path(package.root(), &package.manifest().library.path)?;
    Ok(file.identity)
}

pub(super) fn verify_files(
    package: &PluginPackage,
    host: &HostBuild,
) -> Result<Arc<VerifiedFiles>> {
    let manifest = package.manifest();
    if manifest.rust_build_sha256 != host.rust_build_sha256 || manifest.allocator != host.allocator
    {
        return Err(loader_error(
            ErrorKind::Conflict,
            "matching build and allocator",
        ));
    }
    let main = verify_binary(package, &manifest.library, true)?;
    let mut dependencies = Vec::new();
    dependencies
        .try_reserve_exact(manifest.dependencies.len())
        .map_err(|failure| Error::new(ErrorKind::ResourceExhausted).attach(failure))?;
    for dependency in &manifest.dependencies {
        let file = verify_binary(package, &dependency.file, false)?;
        if file.identity == main.identity {
            return Err(
                loader_error(ErrorKind::Conflict, "plugin also declared as dependency")
                    .attach(PathAttachment(file.path)),
            );
        }
        dependencies.push(file);
    }
    Ok(Arc::new(VerifiedFiles {
        package: package.clone(),
        main,
        dependencies,
    }))
}

fn open_verified_path(root: &Path, declared: &Path) -> Result<VerifiedFile> {
    let candidate = root.join(declared);
    let path = candidate
        .canonicalize()
        .map_err(|failure| io_error(failure, &candidate))?;
    if !path.starts_with(root) {
        return Err(
            loader_error(ErrorKind::InvalidInput, "file outside deployment root")
                .attach(PathAttachment(path)),
        );
    }
    let held_file = open_readonly(&path).map_err(|failure| io_error(failure, &path))?;
    let metadata = held_file
        .metadata()
        .map_err(|failure| io_error(failure, &path))?;
    if !metadata.is_file() {
        return Err(
            loader_error(ErrorKind::InvalidInput, "not a regular deployed file")
                .attach(PathAttachment(path)),
        );
    }
    let identity =
        opened_identity(&held_file, &metadata).map_err(|failure| io_error(failure, &path))?;
    Ok(VerifiedFile {
        path,
        identity,
        held_file,
    })
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
fn open_readonly(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
fn open_readonly(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    File::options()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
fn opened_identity(_: &File, metadata: &std::fs::Metadata) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
fn opened_identity(file: &File, _: &std::fs::Metadata) -> std::io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };
    let mut id = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: 文件存续；输出槽（output slot）足够且独占，仅成功后读取。
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            id.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let id = unsafe { id.assume_init() };
    Ok(FileIdentity::Windows {
        volume_serial: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
    })
}

fn verify_binary(
    package: &PluginPackage,
    expected: &BuildFile,
    plugin: bool,
) -> Result<VerifiedFile> {
    let mut file = open_verified_path(package.root(), &expected.path)?;
    let mut bytes = Vec::new();
    file.held_file
        .read_to_end(&mut bytes)
        .map_err(|failure| io_error(failure, &file.path))?;
    if Sha256::digest(&bytes).as_slice() != expected.sha256 {
        return Err(loader_error(ErrorKind::Conflict, "deployed file digest")
            .attach(PathAttachment(file.path)));
    }
    check_binary(package.manifest(), &bytes, plugin)
        .map_err(|failure| failure.attach(PathAttachment(file.path.clone())))?;
    Ok(file)
}

fn check_binary(manifest: &BuildManifest, bytes: &[u8], plugin: bool) -> Result<()> {
    let binary = object::File::parse(bytes).map_err(binary_error)?;
    let format = match manifest.target {
        Target::LinuxX86_64Gnu => BinaryFormat::Elf,
        Target::WindowsX86_64Msvc => BinaryFormat::Pe,
    };
    if binary.format() != format
        || binary.architecture() != Architecture::X86_64
        || !binary.is_64()
        || binary.kind() != ObjectKind::Dynamic
        || !binary.is_little_endian()
    {
        return Err(loader_error(
            ErrorKind::Unsupported,
            "deployed binary format",
        ));
    }
    if let object::File::Elf64(elf) = &binary {
        use object::read::elf::{Dyn, ProgramHeader};
        let endian = elf.endian();
        for header in elf.elf_program_headers() {
            if let Some(entries) = header.dynamic(endian, bytes).map_err(binary_error)? {
                if entries
                    .iter()
                    .take_while(|entry| entry.tag(endian) != object::elf::DT_NULL)
                    .any(|entry| {
                        entry.tag(endian) == object::elf::DT_FLAGS_1
                            && object::elf::DynamicFlags1(entry.val(endian))
                                .contains(object::elf::DF_1_PIE)
                    })
                {
                    return Err(loader_error(
                        ErrorKind::Unsupported,
                        "ELF position-independent executable",
                    ));
                }
            }
        }
    }
    if plugin {
        check_exports(&binary)?;
    }
    for dependency in binary.import_libraries().map_err(binary_error)? {
        let dependency = dependency.map_err(binary_error)?;
        let name = dependency.name();
        if !manifest
            .dependencies
            .iter()
            .any(|declared| same_load_name(manifest.target, declared.load_name.as_bytes(), name))
            && !system_import(manifest.target, name)
        {
            return Err(
                loader_error(ErrorKind::Dependency, "undeclared non-system import")
                    .attach(String::from_utf8_lossy(name).into_owned()),
            );
        }
    }
    Ok(())
}

fn check_exports(binary: &object::File<'_>) -> Result<()> {
    use object::ObjectSection;
    let expected = [
        METADATA_V1.to_bytes(),
        INITIALIZE_V1.to_bytes(),
        CLOSE_V1.to_bytes(),
    ];
    let mut found = [false; 3];
    for export in binary.exports().map_err(binary_error)? {
        let export = export.map_err(binary_error)?;
        let NameOrOrdinal::Name(name) = export.name() else {
            continue;
        };
        let Some(index) = expected.iter().position(|entry| *entry == name) else {
            continue;
        };
        let published_function = match export.flags() {
            object::ExportFlags::Elf {
                st_info,
                st_other,
                version_hidden,
                ..
            } => {
                if version_hidden {
                    continue;
                }
                st_info.st_type() == object::elf::STT_FUNC
                    && matches!(
                        st_other.visibility(),
                        object::elf::STV_DEFAULT | object::elf::STV_PROTECTED
                    )
            }
            object::ExportFlags::Pe { .. } => true,
            _ => false,
        };
        let direct_function = match export.target() {
            ExportTarget::Address { address } => binary.sections().any(|section| {
                section.kind() == object::SectionKind::Text
                    && address
                        .checked_sub(section.address())
                        .is_some_and(|offset| offset < section.size())
            }),
            _ => false,
        };
        if found[index] || !published_function || !direct_function {
            return Err(
                loader_error(ErrorKind::Corrupted, "invalid direct management export")
                    .attach(String::from_utf8_lossy(name).into_owned()),
            );
        }
        found[index] = true;
    }
    if let Some(index) = found.iter().position(|found| !*found) {
        return Err(
            loader_error(ErrorKind::NotFound, "missing direct management export")
                .attach(String::from_utf8_lossy(expected[index]).into_owned()),
        );
    }
    Ok(())
}

fn same_load_name(target: Target, left: &[u8], right: &[u8]) -> bool {
    match target {
        Target::LinuxX86_64Gnu => left == right,
        Target::WindowsX86_64Msvc => left.eq_ignore_ascii_case(right),
    }
}

fn system_import(target: Target, name: &[u8]) -> bool {
    match target {
        Target::LinuxX86_64Gnu => matches!(
            name,
            b"ld-linux-x86-64.so.2"
                | b"libc.so.6"
                | b"libm.so.6"
                | b"libdl.so.2"
                | b"libpthread.so.0"
                | b"librt.so.1"
                | b"libgcc_s.so.1"
                | b"libutil.so.1"
                | b"libresolv.so.2"
                | b"libanl.so.1"
        ),
        Target::WindowsX86_64Msvc => windows_system_import(name),
    }
}

#[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
fn windows_system_import(name: &[u8]) -> bool {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    // 只借用平台的系统目录（system directory）查询。现有 windows-sys 功能开关未启用此声明。
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, capacity: u32) -> u32;
    }
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if name.contains(['/', '\\', ':', '\0']) || !lower.ends_with(".dll") {
        return false;
    }
    if lower.starts_with("api-ms-win-") || lower.starts_with("ext-ms-win-") {
        return true;
    }
    let mut path = vec![0u16; 32_768];
    // SAFETY: 系统目录输出槽（system directory buffer）可写且长度准确。
    let length = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
    length > 0
        && length < path.len()
        && PathBuf::from(OsString::from_wide(&path[..length]))
            .join(name)
            .is_file()
}

#[cfg(not(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64")))]
fn windows_system_import(_: &[u8]) -> bool {
    false
}

fn loader_error(kind: ErrorKind, operation: &'static str) -> Error {
    Error::new(kind).attach(pi_result::attachment::Operation(operation))
}

fn binary_error(failure: object::Error) -> Error {
    loader_error(ErrorKind::Corrupted, "read native object").attach(failure)
}

fn io_error(failure: std::io::Error, path: &Path) -> Error {
    Error::new(ErrorKind::Io)
        .attach(failure)
        .attach(PathAttachment(path.to_owned()))
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
    files.main.identity
}

pub(super) fn mapped_identity(module: &NativeModule) -> Result<MappedIdentity> {
    todo!(
        "Linux 取得本次活 link_map，Windows 取得本次 HMODULE。系统持有仍在，不解引用任何标准入口。"
    )
}

pub(super) fn verified_manifest(files: &VerifiedFiles) -> &BuildManifest {
    files.package.manifest()
}
