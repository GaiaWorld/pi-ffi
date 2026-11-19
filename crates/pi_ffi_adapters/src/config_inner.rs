use super::*;
use pi_ffi_core::source::ExportedInterface;

pub(super) fn publish(connection: Connection, config: Config) -> Result<PublishedConfig> {
    let source = Arc::new(Registry::attach(connection)?);
    // SAFETY: CONFIG_TABLE 是当前适配库（adapter crate）登记 PendingConfig 的固定表。
    let interface = unsafe { ExportedInterface::new::<ConfigInterface>(&CONFIG_TABLE) }?;
    let object = source.export(
        PendingConfig {
            value: Mutex::new(Some(config)),
        },
        vec![interface],
        |_| Ok(()),
    )?;
    Ok(PublishedConfig { object, source })
}

pub(super) fn install_input<'scope>(
    published: &PublishedConfig,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    let previous = slot.replace(Some(InitializationInput {
        address: std::ptr::from_ref(input),
        source: published.source.clone(),
    }));
    RestoreInput { slot, previous }
}

pub(super) fn bootstrap_input(input: &ObjectRefV1) -> Result<(Arc<Registry>, ObjectRefV1)> {
    let source = current_source(input)?;
    let lease = &input.lease;
    let seed = ObjectRefV1 {
        context: input.context,
        api: input.api,
        lease: LeaseV1 {
            object: pi_ffi_abi::ObjectKeyV1 {
                source: lease.object.source,
                slot: lease.object.slot,
                generation: lease.object.generation,
            },
            initialization: pi_ffi_abi::InitIdV1 {
                module_epoch: lease.initialization.module_epoch,
                round: lease.initialization.round,
            },
            holding: lease.holding,
        },
    };
    Ok((source, seed))
}

pub(super) fn connection<'access>(config: &'access ConfigInput<'_>) -> &'access Connection {
    &config.connection
}

pub(super) fn take(config: &ConfigInput<'_>, lease: LeaseV1) -> Result<Config> {
    let source = current_source(config.input)?;
    if !Arc::ptr_eq(&source, &config.source) {
        return Err(Error::new(ErrorKind::Conflict));
    }
    config
        .source
        .with_access::<PendingConfig, _>(lease, CONFIG_REQUEST, TAKE, |pending| {
            // Mutex 仅保护原配置（Config）的唯一移动（move），诊断及析构均在锁外。
            let value = pending.value.lock().take();
            value.ok_or_else(|| Error::new(ErrorKind::InvalidState))
        })
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    let replaced = restore.slot.replace(restore.previous.take());
    drop(replaced);
}

fn current_source(input: &ObjectRefV1) -> Result<Arc<Registry>> {
    CURRENT_INPUT
        .try_with(|slot| {
            let record = slot
                .try_borrow()
                .map_err(|_| Error::new(ErrorKind::InvalidState))?;
            record
                .as_ref()
                .filter(|record| std::ptr::eq(record.address, input))
                .map(|record| record.source.clone())
                .ok_or_else(|| Error::new(ErrorKind::Conflict))
        })
        .map_err(|_| Error::new(ErrorKind::InvalidState))?
}
