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

fn native_error(operation: &'static str, failure: libloading::Error) -> Error {
    let detail = std::error::Error::source(&failure).map(ToString::to_string);
    let report = loader_error(ErrorKind::Io, operation).attach(failure);
    match detail {
        Some(detail) => report.attach(detail),
        None => report,
    }
}

pub(super) fn open(files: &VerifiedFiles) -> Result<NativeModule> {
    let path = files.main.path.as_path();
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    // SAFETY: 调用方已登记恢复位置，文件来自可信部署（trusted deployment），
    // 装入及释放时运行的本地初始化／终止代码都遵守其配套构建合同。
    let loaded = unsafe { SystemLibrary::open(Some(path), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
    let loaded = {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        };
        // SAFETY: 上层预留了恢复位置，可信部署保证 DLL 的入口及终止代码满足本轮合同。
        unsafe {
            SystemLibrary::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
    };
    let system = loaded.map_err(|failure| {
        native_error("load native module", failure).attach(PathAttachment(path.to_owned()))
    })?;
    let handle = system.into_raw();
    // SAFETY: 无中间可失败操作，将刚移出的同一系统引用（system reference）交回唯一拥有者。
    let system = unsafe { SystemLibrary::from_raw(handle) };
    Ok(NativeModule {
        system: ManuallyDrop::new(system),
        handle,
    })
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
    if raw.uuid.bytes == [0; 16] {
        return Err(loader_error(ErrorKind::Corrupted, "metadata UUID"));
    }
    let length = usize::try_from(raw.version_len)
        .ok()
        .filter(|length| (1..=isize::MAX as usize).contains(length))
        .ok_or_else(|| {
            loader_error(ErrorKind::Corrupted, "metadata version length").attach(raw.version_len)
        })?;
    let end = raw.version.addr().checked_add(length);
    if !end.is_some_and(|end| {
        !raw.version.is_null() && mapped_metadata_range(image.identity, raw.version, end)
    }) {
        return Err(loader_error(ErrorKind::Corrupted, "metadata version range"));
    }
    // SAFETY: 调用方保持本模块的实际系统引用（system reference）。生产方的版本来自同一
    // 不可变字节对象。已核实整段地址、归属及可读范围，借用仅持续到解析结束。
    let bytes = unsafe { std::slice::from_raw_parts(raw.version, length) };
    let version = std::str::from_utf8(bytes).map_err(|failure| {
        loader_error(ErrorKind::Corrupted, "metadata version UTF-8").attach(failure)
    })?;
    let version = semver::Version::parse(version).map_err(|failure| {
        loader_error(ErrorKind::Corrupted, "metadata version SemVer").attach(failure)
    })?;
    Ok(Metadata {
        uuid: raw.uuid.bytes,
        version,
    })
}

pub(super) fn close(module: NativeModule) -> Result<()> {
    // 显式归还系统引用（system reference）。libloading 的 close 消费引用并保留系统错误。
    ManuallyDrop::into_inner(module.system)
        .close()
        .map_err(|failure| native_error("release native module reference", failure))
}

pub(super) fn verified_file_identity(files: &VerifiedFiles) -> FileIdentity {
    files.main.identity
}

pub(super) fn mapped_identity(module: &NativeModule) -> Result<MappedIdentity> {
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    let address = linux_link_map(module)?.addr();
    #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
    let address = module.handle as usize;
    Ok(MappedIdentity { address })
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
fn linux_link_map(module: &NativeModule) -> Result<*mut ElfLinkMapPrefix> {
    let mut mapping = std::ptr::null_mut::<ElfLinkMapPrefix>();
    // SAFETY: NativeModule 仍拥有本次装入引用，link_map 输出槽宽度准确且独占可写。
    let status = unsafe {
        libc::dlinfo(
            module.handle,
            libc::RTLD_DI_LINKMAP,
            std::ptr::from_mut(&mut mapping).cast(),
        )
    };
    if status != 0 {
        let error = loader_error(ErrorKind::Io, "read native link map");
        // SAFETY: 同一线程立即读取 glibc 的本次失败文本，并在下一次平台调用之前复制。
        let message = unsafe {
            let message = libc::dlerror();
            (!message.is_null()).then(|| {
                std::ffi::CStr::from_ptr(message)
                    .to_string_lossy()
                    .into_owned()
            })
        };
        return Err(match message {
            Some(message) => error.attach(message),
            None => error,
        });
    }
    if mapping.is_null() {
        return Err(loader_error(
            ErrorKind::Corrupted,
            "native link map identity",
        ));
    }
    Ok(mapping)
}

pub(super) fn verified_manifest(files: &VerifiedFiles) -> &BuildManifest {
    files.package.manifest()
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
#[repr(C)]
struct ElfLinkMapPrefix {
    load_bias: libc::Elf64_Addr,
    file_name: *const libc::c_char,
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
struct ElfMetadataRange {
    load_bias: libc::Elf64_Addr,
    file_name: *const libc::c_char,
    start: usize,
    end: usize,
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
fn mapped_metadata_range(identity: MappedIdentity, version: *const u8, end: usize) -> bool {
    // glibc 的 dlfcn.h 为此值指定 link_map 输出槽（output slot）。
    const RTLD_DL_LINKMAP: libc::c_int = 2;
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    let mut map = std::ptr::null_mut();
    // SAFETY: dladdr1 按数值查询地址，两个输出槽独占可写且大小准确。
    let found =
        unsafe { libc::dladdr1(version.cast(), info.as_mut_ptr(), &mut map, RTLD_DL_LINKMAP) };
    if found == 0 || map.is_null() || map.addr() != identity.address {
        return false;
    }
    // SAFETY: 查询返回了调用方仍持有的同一 link_map。只读取 GNU 公开布局的前两项。
    let map = unsafe { &*map.cast::<ElfLinkMapPrefix>() };
    let mut range = ElfMetadataRange {
        load_bias: map.load_bias,
        file_name: map.file_name,
        start: version.addr(),
        end,
    };
    // SAFETY: 同步枚举实际映像（mapped image）。区间参数和对应系统引用在回调期间有效。
    unsafe {
        libc::dl_iterate_phdr(
            Some(readable_elf_metadata_range),
            std::ptr::from_mut(&mut range).cast(),
        ) == 1
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
unsafe extern "C" fn readable_elf_metadata_range(
    info: *mut libc::dl_phdr_info,
    size: usize,
    data: *mut core::ffi::c_void,
) -> libc::c_int {
    if size < size_of::<libc::dl_phdr_info>() {
        return 0;
    }
    // SAFETY: 同步枚举（enumeration）提供本次映像描述及原样传回调用方的区间参数。
    let (info, range) = unsafe { (&*info, &*data.cast::<ElfMetadataRange>()) };
    if info.dlpi_addr != range.load_bias
        || info.dlpi_name.is_null()
        || range.file_name.is_null()
        || info.dlpi_phdr.is_null()
    {
        return 0;
    }
    // SAFETY: 两个名称来自仍有效的系统记录，均以零字节结束。
    let names_match = unsafe {
        std::ffi::CStr::from_ptr(info.dlpi_name) == std::ffi::CStr::from_ptr(range.file_name)
    };
    if !names_match {
        return 0;
    }
    // SAFETY: 枚举接口保证程序头（program header）表在回调内有效，条数取自同一记录。
    let headers =
        unsafe { std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) };
    let mut cursor = range.start;
    // ELF 按虚拟地址（virtual address）从低到高列出加载段。字节范围只能由可读加载段连续覆盖。
    for header in headers {
        if header.p_type != libc::PT_LOAD || header.p_flags & libc::PF_R == 0 {
            continue;
        }
        // GNU 的装入偏移（load bias）是无符号地址差，可用回绕运算表示负偏移。
        let start = (info.dlpi_addr as usize).wrapping_add(header.p_vaddr as usize);
        let Some(end) = start.checked_add(header.p_memsz as usize) else {
            continue;
        };
        if start <= cursor && cursor < end {
            cursor = end;
            if cursor >= range.end {
                return 1;
            }
        }
    }
    0
}

#[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
fn mapped_metadata_range(identity: MappedIdentity, version: *const u8, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE, PAGE_EXECUTE_READ,
        PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE,
        PAGE_WRITECOPY,
    };
    let readable = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    let mut cursor = version.addr();
    let mut region = MEMORY_BASIC_INFORMATION::default();
    while cursor < end {
        // SAFETY: VirtualQuery 按数值查询地址。输出槽（output slot）独占可写且大小准确。
        let written = unsafe {
            VirtualQuery(
                std::ptr::without_provenance(cursor),
                &mut region,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        let start = region.BaseAddress.addr();
        if written != size_of::<MEMORY_BASIC_INFORMATION>()
            || region.AllocationBase.addr() != identity.address
            || region.State != MEM_COMMIT
            || region.Type != MEM_IMAGE
            || region.Protect & readable == 0
            || region.Protect & PAGE_GUARD != 0
            || start > cursor
        {
            return false;
        }
        let Some(next) = start
            .checked_add(region.RegionSize)
            .filter(|&next| next > cursor)
        else {
            return false;
        };
        cursor = next;
    }
    true
}
