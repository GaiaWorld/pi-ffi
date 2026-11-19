use super::*;
use pi_ffi_abi::ObjectKeyV1;
use pi_ffi_core::source::ExportedInterface;

pub(super) fn publish<R>(connection: Connection, runtime: Arc<R>) -> Result<PublishedRuntime>
where
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    let source = Arc::new(Registry::attach(connection)?);
    // SAFETY: RUNTIME_TABLE 为本 Rust 适配库（adapter crate）登记 Arc<R> 使用的固定表。
    let interface = unsafe { ExportedInterface::new::<RuntimeInterface>(&RUNTIME_TABLE) }?;
    let object = source.export(runtime, vec![interface], |_| Ok(()))?;
    Ok(PublishedRuntime { object, source })
}

pub(super) fn install_input<'scope>(
    published: &PublishedRuntime,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    let lease = &input.lease;
    let previous = slot.replace(Some(InitializationInput {
        lease: LeaseV1 {
            object: ObjectKeyV1 {
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
                .filter(|record| same_lease(&record.lease, &input.lease))
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

pub(super) fn with_runtime_result<V>(
    result: Result<()>,
    output: Option<V>,
) -> RawResult<V, (Error, Option<V>)> {
    match (result, output) {
        (Ok(()), Some(output)) => Ok(output),
        (Ok(()), None) => Err((Error::new(ErrorKind::Corrupted), None)),
        (Err(error), output) => Err((error, output)),
    }
}

pub(super) fn clone_runtime<R>(runtime: &Runtime<R>) -> Runtime<R> {
    Runtime {
        interface: runtime.interface.clone(),
        source: runtime.source.clone(),
        runtime: PhantomData,
    }
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    let replaced = restore.slot.replace(restore.previous.take());
    drop(replaced);
}

fn same_lease(left: &LeaseV1, right: &LeaseV1) -> bool {
    left.object.source == right.object.source
        && left.object.slot == right.object.slot
        && left.object.generation == right.object.generation
        && left.initialization.module_epoch == right.initialization.module_epoch
        && left.initialization.round == right.initialization.round
        && left.holding == right.holding
}

pub(super) fn view_create_with_config<R: TaskDispatch, F, M>(
    runtime: &R,
    future: F,
    config: TaskConfig<M>,
) -> RawResult<(UnsubmittedTask, TaskHandle<R::Notifier, F::Output, M>), CreateError<F, M>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    M: Send + Sync + 'static,
{
    R::create_with_config(runtime, future, config)
}

pub(super) fn view_submit<R: TaskDispatch>(
    runtime: &R,
    task: UnsubmittedTask,
) -> RawResult<(), SubmitError> {
    R::submit(runtime, task)
}

pub(super) fn view_create<R: TaskDispatch, F>(
    runtime: &R,
    future: F,
) -> RawResult<(UnsubmittedTask, TaskHandle<R::Notifier, F::Output>), CreateError<F, ()>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    R::create(runtime, future)
}

pub(super) fn view_spawn_with_config<R: TaskDispatch, F, M>(
    runtime: &R,
    future: F,
    config: TaskConfig<M>,
) -> RawResult<TaskHandle<R::Notifier, F::Output, M>, SpawnError<F, R::Notifier, M>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    M: Send + Sync + 'static,
{
    R::spawn_with_config(runtime, future, config)
}

pub(super) fn view_spawn<R: TaskDispatch, F>(
    runtime: &R,
    future: F,
) -> RawResult<TaskHandle<R::Notifier, F::Output>, SpawnError<F, R::Notifier>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    R::spawn(runtime, future)
}

pub(super) fn view_submit_idle<R: IdleDispatch>(
    runtime: &R,
    task: UnsubmittedTask,
) -> RawResult<(), SubmitError> {
    R::submit_idle(runtime, task)
}

pub(super) fn view_idle_with_config<R: IdleDispatch, F, M>(
    runtime: &R,
    future: F,
    config: TaskConfig<M>,
) -> RawResult<TaskHandle<R::Notifier, F::Output, M>, SpawnError<F, R::Notifier, M>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    M: Send + Sync + 'static,
{
    R::idle_with_config(runtime, future, config)
}

pub(super) fn view_idle<R: IdleDispatch, F>(
    runtime: &R,
    future: F,
) -> RawResult<TaskHandle<R::Notifier, F::Output>, SpawnError<F, R::Notifier>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    R::idle(runtime, future)
}

pub(super) fn view_idle_local_with_config<R: IdleDispatch, F, M>(
    future: F,
    config: TaskConfig<M>,
) -> RawResult<TaskHandle<R::Notifier, F::Output, M>, SpawnError<F, R::Notifier, M>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    M: Send + Sync + 'static,
{
    R::idle_local_with_config(future, config)
}

pub(super) fn view_idle_local<R: IdleDispatch, F>(
    future: F,
) -> RawResult<TaskHandle<R::Notifier, F::Output>, SpawnError<F, R::Notifier>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    R::idle_local(future)
}

pub(super) fn view_timeout<R: RuntimeTime>(runtime: &R, duration: Duration) -> Timeout<R::Timer> {
    R::timeout(runtime, duration)
}

pub(super) fn view_timeout_on<R: RuntimeTime, F: Future>(
    runtime: &R,
    future: F,
    duration: Duration,
) -> TimeoutOn<R::Timer, F> {
    R::timeout_on(runtime, future, duration)
}

pub(super) fn view_sleep<R: RuntimeTime>(runtime: &R, duration: Duration) -> Sleep<R::Timer> {
    R::sleep(runtime, duration)
}

pub(super) fn view_wait_one_timeout<R: RuntimeWait, T>(
    runtime: &R,
    duration: Duration,
) -> (WaitOneTimeout<R::Timer, T>, OneSender<T>) {
    R::wait_one_timeout(runtime, duration)
}

pub(super) fn view_wait_any_timeout<R: RuntimeWait, T>(
    runtime: &R,
    duration: Duration,
) -> (WaitAnyTimeout<R::Timer, T>, AnySender<T>) {
    R::wait_any_timeout(runtime, duration)
}

pub(super) fn view_wait_all_timeout<R: RuntimeWait, T>(
    runtime: &R,
    duration: Duration,
) -> (WaitAllTimeout<R::Timer, T>, AllSender<T>) {
    R::wait_all_timeout(runtime, duration)
}

pub(super) fn view_id<R: RuntimeQuery>(runtime: &R) -> RuntimeId {
    R::id(runtime)
}

pub(super) fn view_status<R: RuntimeQuery>(runtime: &R) -> RuntimeStatus {
    R::status(runtime)
}

pub(super) fn view_is_driving<R: RuntimeQuery>(runtime: &R) -> bool {
    R::is_driving(runtime)
}

pub(super) fn view_last_idle_state<R: RuntimeQuery>(runtime: &R) -> IdleState {
    R::last_idle_state(runtime)
}

pub(super) fn view_close_state<R: RuntimeQuery>(runtime: &R) -> CloseState {
    R::close_state(runtime)
}

pub(super) fn view_external_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::external_task_count(runtime)
}

pub(super) fn view_internal_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::internal_task_count(runtime)
}

pub(super) fn view_timer_registration_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::timer_registration_count(runtime)
}

pub(super) fn view_idle_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::idle_task_count(runtime)
}

pub(super) fn view_in_flight_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::in_flight_task_count(runtime)
}

pub(super) fn view_waiting_for_wake_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::waiting_for_wake_count(runtime)
}

pub(super) fn view_pending_cancel_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    R::pending_cancel_task_count(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 原生返回值（native result）和 B1 调用保护的结果分别保留。
    #[test]
    fn test_runtime_result_preserves_native_output_and_cleanup_error() {
        let value = String::from("原生返回值（native result）");
        let original = Err::<(), _>(&value);
        let successful_access = with_runtime_result(Ok(()), Some(original)).unwrap();
        assert!(std::ptr::eq(successful_access.unwrap_err(), &value));

        let cleanup_failure = Error::new(ErrorKind::Io).attach(27u32);
        let (error, returned) =
            with_runtime_result(Err(cleanup_failure), Some(original)).unwrap_err();
        assert_eq!(error.current_context(), &ErrorKind::Io);
        assert_eq!(error.downcast_ref::<u32>(), Some(&27));
        assert!(std::ptr::eq(returned.unwrap().unwrap_err(), &value));

        let refused = Error::new(ErrorKind::Busy);
        let (error, returned) = with_runtime_result::<()>(Err(refused), None).unwrap_err();
        assert_eq!(error.current_context(), &ErrorKind::Busy);
        assert_eq!(returned, None);

        let (error, returned) = with_runtime_result::<()>(Ok(()), None).unwrap_err();
        assert_eq!(error.current_context(), &ErrorKind::Corrupted);
        assert_eq!(returned, None);
    }
}
