use super::*;
use pi_ffi_core::source::ExportedInterface;

pub(super) fn publish<T>(connection: Connection, terminal: Arc<T>) -> Result<PublishedTerminal>
where
    T: Send + Sync + 'static,
{
    let source = Arc::new(Registry::attach(connection)?);
    // SAFETY: TERMINAL_TABLE 是当前适配库（adapter crate）登记原生终端共享持有（shared holding）的固定表。
    let interface = unsafe { ExportedInterface::new::<TerminalInterface>(&TERMINAL_TABLE) }?;
    let object = source.export(terminal, vec![interface], |_| Ok(()))?;
    Ok(PublishedTerminal { object, source })
}

pub(super) fn install_input<'scope>(
    published: &PublishedTerminal,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    let previous = slot.replace(Some(InitializationInput {
        address: std::ptr::from_ref(input),
        source: published.source.clone(),
    }));
    RestoreInput { slot, previous }
}

pub(super) fn source_for_input(
    connection: &Connection,
    input: &ObjectRefV1,
) -> Result<Arc<Registry>> {
    let source = CURRENT_INPUT
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
        .map_err(|_| Error::new(ErrorKind::InvalidState))??;
    let round = connection.initialization();
    if round.module_epoch != input.lease.initialization.module_epoch
        || round.round != input.lease.initialization.round
    {
        return Err(Error::new(ErrorKind::Conflict));
    }
    Ok(source)
}

pub(super) fn native<T>(terminal: &Terminal<T>) -> &T {
    terminal.terminal.as_ref()
}

pub(super) fn clone_terminal<T>(terminal: &Terminal<T>) -> Terminal<T> {
    Terminal {
        terminal: terminal.terminal.clone(),
        holding: terminal.holding.clone(),
    }
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    let replaced = restore.slot.replace(restore.previous.take());
    drop(replaced);
}

pub(super) fn register_static<T, D>(
    terminal: &T,
    declarations: impl IntoIterator<Item = D>,
    register: impl FnOnce(&T, Vec<D>) -> Result<()>,
) -> Result<()> {
    let declarations = declarations.into_iter();
    let mut collected = Vec::new();
    let allocation_failure = |failure| Error::new(ErrorKind::ResourceExhausted).attach(failure);
    collected
        .try_reserve(declarations.size_hint().0)
        .map_err(allocation_failure)?;
    for declaration in declarations {
        collected.try_reserve(1).map_err(allocation_failure)?;
        collected.push(declaration);
    }
    if collected.is_empty() {
        return Ok(());
    }
    register(terminal, collected)
}
