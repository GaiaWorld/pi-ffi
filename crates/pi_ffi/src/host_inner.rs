use super::*;
use crate::{authority::LoadAdmission, loader};
use pi_ffi_abi::management::MetadataV1;
use pi_ffi_core::{boundary, diagnostic};
use pi_result::{attachment::Operation as ErrorOperation, Error, ErrorKind};
use std::mem::MaybeUninit;

pub(super) unsafe fn terminal_input<T: Send + Sync + 'static>(
    terminal: Arc<T>,
) -> TerminalInput<T> {
    TerminalInput { terminal }
}

pub(super) fn manager_process(build: HostBuild) -> Result<Manager> {
    Authority::process(build).map(|authority| Manager { authority })
}

pub(super) fn manager_find(
    manager: &Manager,
    packages: &[PluginPackage],
    request: &PluginRequest,
) -> Result<PluginPackage> {
    let _ = manager;
    let mut matching = packages.iter().filter(|package| {
        let manifest = package.manifest();
        manifest.uuid == request.uuid && request.version.matches(&manifest.version)
    });
    let best = matching
        .clone()
        .max_by(|left, right| {
            left.manifest()
                .version
                .cmp_precedence(&right.manifest().version)
        })
        .ok_or_else(|| Error::new(ErrorKind::NotFound).attach(request.version.to_string()))?;
    let selected_identity = loader::file_identity(best)?;
    let selected = best.manifest();
    for package in &mut matching {
        let manifest = package.manifest();
        if std::ptr::eq(package, best)
            || !manifest.version.cmp_precedence(&selected.version).is_eq()
        {
            continue;
        }
        if loader::file_identity(package)? != selected_identity
            || manifest.version != selected.version
            || manifest.library.sha256 != selected.library.sha256
            || manifest.rust_build_sha256 != selected.rust_build_sha256
            || manifest.allocator != selected.allocator
        {
            return Err(Error::new(ErrorKind::Conflict).attach(selected.version.to_string()));
        }
    }
    Ok(best.clone())
}

pub(super) fn manager_load(
    manager: &Manager,
    package: &PluginPackage,
    request: &PluginRequest,
) -> Result<Plugin> {
    let manifest = package.manifest();
    if manifest.uuid != request.uuid || !request.version.matches(&manifest.version) {
        return Err(Error::new(ErrorKind::NotFound)
            .attach(ErrorOperation("match plugin load request"))
            .attach(request.version.to_string()));
    }

    let authority = manager.authority;
    let build = authority.build()?;
    let files = loader::verify_files(package, &build)?;
    let view = match authority.reserve_load(Arc::clone(&files))? {
        LoadAdmission::Reuse(view) => {
            check_metadata(&view.metadata, manifest, request)?;
            view
        }
        LoadAdmission::New(mut operation) => {
            let epoch = operation.epoch();
            let inspected = boundary::catch(
                boundary::Context {
                    operation: "load plugin",
                    module_epoch: epoch,
                    round: 0,
                    source: 0,
                },
                || {
                    operation.open()?;
                    let module = operation.library()?;
                    let mapped = loader::mapped_identity(module)?;
                    authority.record_mapping(epoch, mapped)?;
                    let image = loader::verify_image(module, files)?;
                    let mut raw = MaybeUninit::<MetadataV1>::uninit();
                    // 安全前提（safety）。标准入口已核实归属于本次持有的模块，输出槽独占可写。
                    let outcome = unsafe { (image.entries.metadata)(raw.as_mut_ptr()) };
                    // 安全前提（safety）。仍保有同一系统引用。元信息入口没有已登记诊断来源。
                    unsafe { diagnostic::decode_management(None, outcome) }.map_err(|failure| {
                        failure.attach(ErrorOperation("read plugin metadata"))
                    })?;
                    // 安全前提（safety）。标准入口成功后完整写入槽位，复制持续借用原系统引用。
                    let metadata = unsafe { loader::copy_metadata(&image, raw.assume_init()) }?;
                    check_metadata(&metadata, manifest, request)?;
                    Ok((image, metadata))
                },
            );
            match inspected {
                Ok((image, metadata)) => operation.confirm_loaded(image, metadata)?,
                Err(failure) => return Err(operation.reject(failure)),
            }
        }
    };
    Ok(Plugin {
        authority,
        epoch: view.epoch,
        metadata: view.metadata,
    })
}

fn check_metadata(
    metadata: &Metadata,
    manifest: &BuildManifest,
    request: &PluginRequest,
) -> Result<()> {
    if metadata.uuid == manifest.uuid
        && metadata.version == manifest.version
        && metadata.uuid == request.uuid
        && request.version.matches(&metadata.version)
    {
        return Ok(());
    }
    Err(Error::new(ErrorKind::Conflict)
        .attach(ErrorOperation("match mapped plugin metadata"))
        .attach(format!("实际 UUID（actual UUID） {:02x?}", metadata.uuid))
        .attach(format!("批准 UUID（approved UUID） {:02x?}", manifest.uuid))
        .attach(format!("实际版本（actual version） {}", metadata.version))
        .attach(format!("批准版本（approved version） {}", manifest.version)))
}

pub(super) fn manager_maintain(manager: &Manager, budget: usize) -> Result<usize> {
    todo!("在现有责任内取有界工作，锁外调用来源或执行已知尚未进行的系统释放。未知结果保留；不驱动插件或整个运行时。")
}

pub(super) fn plugin_metadata(plugin: &Plugin) -> &Metadata {
    &plugin.metadata
}

pub(super) fn plugin_prepare<T, R>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
    runtime: Option<Arc<R>>,
) -> Result<Initialization>
where
    T: Send + Sync + 'static,
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    todo!("把本轮实际有无运行时交给 Authority::reserve_round。取得已核配套、已登记本轮的连接后，按 B2—B4 装包，每项只登记一次。登记后的失败进入明确的宿主归还路径。")
}

pub(super) fn plugin_prepare_without_runtime<T: Send + Sync + 'static>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
) -> Result<Initialization> {
    todo!("沿同一轮次准备逻辑，将无运行时传给 Authority::reserve_round。获得连接后只登记配置和终端。")
}

pub(super) fn plugin_current_initialization(plugin: &Plugin) -> Result<Option<Initialization>> {
    todo!("核本模块代际，只返回当前本地管理入口，不查询插件的业务数据。")
}

pub(super) fn plugin_unload(plugin: &Plugin) -> Result<()> {
    if let Some(operation) = plugin.authority.prepare_unload(plugin.epoch)? {
        operation.release()?;
    }
    Ok(())
}

pub(super) fn initialization_id(initialization: &Initialization) -> InitIdV1 {
    InitIdV1 {
        module_epoch: initialization.epoch,
        round: initialization.round,
    }
}

pub(super) fn initialization_run(initialization: &Initialization) -> Result<()> {
    todo!("取得同一轮次调用资格，核实实际依赖，用配置外层保护及终端、可选运行时借用同步调用。先记效果先由 Authority 核本模块诊断登记，再处理 B1 诊断，最后归还实际保护。")
}

pub(super) fn initialization_close(initialization: &Initialization) -> Result<()> {
    todo!("不等待在途调用。插件明确完成后保存回执，在本模块持有内校验并接管诊断，再推进 B1 归还。已成功部分不重跑，旧轮只回放原结果。")
}
