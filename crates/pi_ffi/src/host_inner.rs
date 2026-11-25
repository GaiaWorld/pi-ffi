use super::*;
use crate::{
    authority::{FfiProgress, LoadAdmission, Operation, PreparedInputs, RoundAdmission, RoundCall},
    loader,
};
use pi_ffi_abi::{
    management::{MetadataV1, INIT_COMPLETED, INIT_NEEDS_CLOSE, INIT_NOT_STARTED},
    ObjectRefV1,
};
use pi_ffi_adapters::{
    config::PublishedConfig, data::PublishedData, runtime::PublishedRuntime, service::PublishedTerminal,
};
use pi_ffi_core::{boundary, control::Connection, diagnostic};
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
            || package.unload_policy() != best.unload_policy()
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
                    operation.record_metadata(&metadata)?;
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
    manager.authority.maintain(None, budget)
}

pub(super) fn plugin_metadata(plugin: &Plugin) -> &Metadata {
    &plugin.metadata
}

pub(super) fn plugin_prepare<T, R, D>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
    runtime: Option<Arc<R>>,
    data: D,
) -> Result<Initialization>
where
    T: Send + Sync + 'static,
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
    D: Send + Sync + 'static,
{
    prepare_plugin(plugin, terminal, config, data, runtime.is_some(), |connection| {
        runtime
            .map(|runtime| PublishedRuntime::publish(connection, runtime))
            .transpose()
    })
}

pub(super) fn plugin_prepare_without_runtime<T: Send + Sync + 'static, D: Send + Sync + 'static>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
    data: D,
) -> Result<Initialization> {
    prepare_plugin(plugin, terminal, config, data, false, |_| Ok(None))
}

fn prepare_plugin<T: Send + Sync + 'static, D: Send + Sync + 'static>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
    data: D,
    has_runtime: bool,
    publish_runtime: impl FnOnce(Connection) -> Result<Option<PublishedRuntime>>,
) -> Result<Initialization> {
    let authority = plugin.authority;
    let mut reserved = None;
    let prepared = boundary::catch(
        boundary::Context {
            operation: "publish plugin initialization inputs",
            module_epoch: plugin.epoch,
            round: 0,
            source: 0,
        },
        || {
            // 原值的接管及准备失败时的析构（Drop）均位于同一保护边界内。
            let (id, connection) = authority.reserve_round(plugin.epoch, has_runtime)?;
            reserved = Some(InitIdV1 { module_epoch: id.module_epoch, round: id.round });
            // 安全前提（safety）。同轮连接及实际配套已核对，原生输入由其配套代码持有。
            let config = unsafe { PublishedConfig::publish(connection.clone(), config) }?;
            let terminal =
                unsafe { PublishedTerminal::publish(connection.clone(), terminal.terminal) }?;
            let data = unsafe { PublishedData::publish(connection.clone(), data) }?;
            let runtime = publish_runtime(connection)?;
            authority.publish_inputs(
                InitIdV1 {
                    module_epoch: id.module_epoch,
                    round: id.round,
                },
                PreparedInputs {
                    config,
                    terminal,
                    runtime,
                    data,
                },
            )
        },
    );
    match (prepared, reserved) {
        (Ok(()), Some(id)) => Ok(Initialization {
            authority,
            epoch: id.module_epoch,
            round: id.round,
        }),
        (Err(failure), Some(id)) => Err(authority.fail_prepare(id, failure)),
        (Err(failure), None) => Err(failure),
        (Ok(()), None) => Err(Error::new(ErrorKind::Corrupted)),
    }
}

pub(super) fn plugin_current_initialization(plugin: &Plugin) -> Result<Option<Initialization>> {
    let authority = plugin.authority;
    authority.current_round(plugin.epoch).map(|current| {
        current.map(|id| Initialization {
            authority,
            epoch: id.module_epoch,
            round: id.round,
        })
    })
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
    let authority = initialization.authority;
    let id = initialization.id();
    match authority.begin_round(id, Operation::Initialize)? {
        RoundAdmission::Replay { error } => replay_round(error),
        RoundAdmission::ReturnHoldings => Err(Error::new(ErrorKind::InvalidState)
            .attach(ErrorOperation("initialize returning plugin inputs"))),
        RoundAdmission::Call(call) => {
            let result = boundary::catch(
                initialization_context(initialization, "initialize plugin"),
                || initialize_with_inputs(authority, &call),
            );
            authority.finish_round(call, result)
        }
    }
}

pub(super) fn initialization_close(initialization: &Initialization) -> Result<()> {
    let authority = initialization.authority;
    let id = initialization.id();
    match authority.begin_round(id, Operation::Close)? {
        RoundAdmission::Replay { error } => replay_round(error),
        RoundAdmission::ReturnHoldings => finish_returning_inputs(initialization),
        RoundAdmission::Call(call) => {
            let mut completed = false;
            let result = boundary::catch(
                initialization_context(initialization, "close plugin"),
                || {
                    authority.remember_progress(&call, FfiProgress::OutcomeUnknown);
                    // 安全前提（safety）。本次独占标准调用保活同一实际模块，编号为本轮。
                    let outcome = unsafe { (call.entries.close)(initialization.id()) };
                    completed = valid_close_success(&outcome);
                    if completed {
                        authority.remember_progress(&call, FfiProgress::CloseCompleted);
                    } else if outcome.kind == diagnostic::status_code(ErrorKind::Busy) {
                        authority.remember_progress(&call, FfiProgress::ClosePending);
                    }
                    authority.validate_diagnostic(&call, &outcome)?;
                    // 安全前提（safety）。本模块、此轮已登记来源及独立诊断均仍有实际保护。
                    unsafe { diagnostic::decode_management(Some(&call.connection), outcome) }
                },
            );
            let finished = authority.finish_round(call, result);
            if completed && finished.is_ok() {
                finish_returning_inputs(initialization)
            } else {
                finished
            }
        }
    }
}

fn initialization_context(
    initialization: &Initialization,
    operation: &'static str,
) -> boundary::Context {
    boundary::Context {
        operation,
        module_epoch: initialization.epoch,
        round: initialization.round,
        source: 0,
    }
}

fn replay_round(error: Option<Arc<Error>>) -> Result<()> {
    match error {
        None => Ok(()),
        Some(error) => {
            let nested = error
                .downcast_ref::<Arc<diagnostic::CopiedDiagnostic>>()
                .map(Arc::clone);
            let report = Error::new(*error.current_context())
                .attach(ErrorOperation("replay plugin initialization round"))
                .attach(pi_result::debug_message(&error));
            Err(match nested {
                Some(nested) => report.attach(nested),
                None => report,
            })
        }
    }
}

fn initialize_with_inputs(authority: &Authority, call: &RoundCall) -> Result<()> {
    let inputs = call
        .inputs
        .as_ref()
        .ok_or_else(|| Error::new(ErrorKind::Corrupted))?;
    loader::check_inputs(&call.image, inputs.runtime.is_some())?;
    inputs.config.with_input(|config| {
        inputs.data.with_input(|data| inputs
            .terminal
            .with_input(|terminal| match &inputs.runtime {
                Some(runtime) => runtime.with_input(|runtime| {
                    invoke_initialization(
                        authority,
                        call,
                        std::ptr::from_ref(runtime),
                        terminal,
                        config,
                        data,
                    )
                }),
                None => invoke_initialization(authority, call, std::ptr::null(), terminal, config, data),
            }))
    })
}

fn invoke_initialization(
    authority: &Authority,
    call: &RoundCall,
    runtime: *const ObjectRefV1,
    terminal: &ObjectRefV1,
    config: &ObjectRefV1,
    data: &ObjectRefV1,
) -> Result<()> {
    authority.remember_progress(call, FfiProgress::OutcomeUnknown);
    // 安全前提（safety）。标准调用资格保活实际模块，所有原生输入处在嵌套同步借用中。
    let reply = unsafe { (call.entries.initialize)(runtime, terminal, config, data) };
    let progress = match reply.progress {
        INIT_COMPLETED => FfiProgress::InitializationCompleted,
        INIT_NOT_STARTED if reply.outcome.kind != 0 => FfiProgress::InitializationNotStarted,
        INIT_NEEDS_CLOSE if reply.outcome.kind != 0 => FfiProgress::InitializationNeedsClose,
        _ => {
            return Err(Error::new(ErrorKind::Corrupted)
                .attach(ErrorOperation("read initialization progress"))
                .attach(reply.progress))
        }
    };
    authority.remember_progress(call, progress);
    authority.validate_diagnostic(call, &reply.outcome)?;
    // 安全前提（safety）。独立诊断已经核对本模块、来源和本轮持有，所有借用仍有效。
    unsafe { diagnostic::decode_management(Some(&call.connection), reply.outcome) }
}

fn valid_close_success(outcome: &pi_ffi_abi::OutcomeV1) -> bool {
    let diagnostic = &outcome.diagnostic;
    let lease = &diagnostic.lease;
    outcome.kind == 0
        && outcome.diagnostic_failure_kind == 0
        && diagnostic.context.is_null()
        && diagnostic.api.is_null()
        && lease.object.source == 0
        && lease.object.slot == 0
        && lease.object.generation == 0
        && lease.initialization.module_epoch == 0
        && lease.initialization.round == 0
        && lease.holding == 0
}

fn finish_returning_inputs(initialization: &Initialization) -> Result<()> {
    let authority = initialization.authority;
    authority.maintain(Some(initialization.id()), 64)?;
    match authority.begin_round(initialization.id(), Operation::Close)? {
        RoundAdmission::Replay { error } => replay_round(error),
        RoundAdmission::ReturnHoldings => Err(Error::new(ErrorKind::Busy)
            .attach(ErrorOperation("return plugin initialization inputs"))),
        RoundAdmission::Call(_) => Err(Error::new(ErrorKind::Corrupted)
            .attach(ErrorOperation("repeat completed plugin close"))),
    }
}
