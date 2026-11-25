use super::*;
use pi_ffi_core::source::ExportedInterface;

pub(super) fn publish<D: Send + Sync + 'static>(
    connection: Connection,
    data: D,
) -> Result<PublishedData> {
    let source = Arc::new(Registry::attach(connection)?);
    // SAFETY: 本适配库固定只读表对应本次实际登记的 PendingData<D>。
    let interface = unsafe { ExportedInterface::new::<DataInterface>(&DATA_TABLE) }?;
    let object = source.export(
        PendingData {
            value: Mutex::new(Some(data)),
        },
        vec![interface],
        |_| Ok(()),
    )?;
    Ok(PublishedData { object, source })
}

pub(super) fn install_input<'scope>(
    published: &PublishedData,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    let previous = slot.replace(Some(InitializationInput {
        address: std::ptr::from_ref(input),
        source: published.source.clone(),
    }));
    RestoreInput { slot, previous }
}

pub(super) unsafe fn receive<'call, D: Send + Sync + 'static>(
    connection: &Connection,
    input: &'call ObjectRefV1,
) -> Result<DataInput<'call, D>> {
    let source = current_source(input)?;
    // SAFETY: 原始借用和本轮连接受外层保护；retain_borrowed 核真实同轮管理视图。
    let object = unsafe { Object::retain_borrowed(connection, input) }?;
    let interface = object.interface::<DataInterface>()?;
    interface.with_call(READ, |call| {
        source.with_access::<PendingData<D>, _>(call.lease(), DATA_REQUEST, READ, |_| Ok(()))
    })?;
    Ok(DataInput {
        input,
        interface,
        source,
        marker: PhantomData,
    })
}

pub(super) fn take_into<D: Send + Sync + 'static>(
    data: &DataInput<'_, D>,
    lease: LeaseV1,
    output: &mut Option<D>,
) -> Result<()> {
    let source = current_source(data.input)?;
    if !Arc::ptr_eq(&source, &data.source) {
        return Err(Error::new(ErrorKind::Conflict));
    }
    data.source
        .with_access::<PendingData<D>, _>(lease, DATA_REQUEST, TAKE, |pending| {
            // 先释放短锁再处理错误或移动原值；调用保护的闭包只返回 ()。
            let value = pending.value.lock().take();
            *output = Some(value.ok_or_else(|| Error::new(ErrorKind::InvalidState))?);
            Ok(())
        })
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    let replaced = restore.slot.replace(restore.previous.take());
    drop(replaced);
}

fn current_source(input: &ObjectRefV1) -> Result<Arc<Registry>> {
    PI_FFI_ADAPTERS_DATA_INPUT
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
