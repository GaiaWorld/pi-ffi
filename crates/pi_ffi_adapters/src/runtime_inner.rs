// 不使用参数是设计占位（stub）的临时事实。
#![allow(unused_variables, dead_code)]

use super::*;

pub(super) fn publish<R>(connection: Connection, runtime: Arc<R>) -> Result<PublishedRuntime>
where
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    todo!("在本轮 Connection 附着来源，登记原 Arc<R>、只读表头和适配输入清理回调；成功立即包装 Object，失败由 B1 保留待归还责任")
}

pub(super) fn install_input<'scope>(
    published: &PublishedRuntime,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    todo!("从受保护输入逐字段复制完整 LeaseV1，连同同一实际 Registry 安装于本线程槽位；把上一记录移给恢复守卫，返回前结束短期可变借用")
}

pub(super) fn source_for_input(
    connection: &Connection,
    input: &ObjectRefV1,
) -> Result<Arc<Registry>> {
    todo!("只检查当前线程最内层记录和完整 LeaseV1，槽位不可用报 InvalidState，缺失或不符报 Conflict；核连接轮次，不符报 Conflict；在返回源引用前结束槽位借用")
}

pub(super) fn with_runtime_result<V>(
    result: Result<()>,
    output: Option<V>,
) -> RawResult<V, (Error, Option<V>)> {
    todo!("B1 失败时原样交付 pi_result 和可能已有的原值；确认成功时交付原值，反常成功却缺失原值时报告 Corrupted")
}

pub(super) fn clone_runtime<R>(runtime: &Runtime<R>) -> Runtime<R> {
    todo!("共享现有 Interface 的本地拥有者及同一 Registry，不建立独立 C 持有")
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    todo!("利用守卫已借用的槽位恢复原记录，短期借用结束后释放换出的记录，不从 TLS 再查询或调用外部代码")
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
    todo!("调用 `R::create_with_config` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_submit<R: TaskDispatch>(
    runtime: &R,
    task: UnsubmittedTask,
) -> RawResult<(), SubmitError> {
    todo!("调用 `R::submit` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_create<R: TaskDispatch, F>(
    runtime: &R,
    future: F,
) -> RawResult<(UnsubmittedTask, TaskHandle<R::Notifier, F::Output>), CreateError<F, ()>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    todo!("调用 `R::create` 一次，按其原生契约交付参数与结果")
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
    todo!("调用 `R::spawn_with_config` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_spawn<R: TaskDispatch, F>(
    runtime: &R,
    future: F,
) -> RawResult<TaskHandle<R::Notifier, F::Output>, SpawnError<F, R::Notifier>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    todo!("调用 `R::spawn` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_submit_idle<R: IdleDispatch>(
    runtime: &R,
    task: UnsubmittedTask,
) -> RawResult<(), SubmitError> {
    todo!("调用 `R::submit_idle` 一次，按其原生契约交付参数与结果")
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
    todo!("调用 `R::idle_with_config` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_idle<R: IdleDispatch, F>(
    runtime: &R,
    future: F,
) -> RawResult<TaskHandle<R::Notifier, F::Output>, SpawnError<F, R::Notifier>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    todo!("调用 `R::idle` 一次，按其原生契约交付参数与结果")
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
    todo!("调用 `R::idle_local_with_config`，遵守提供方当前本地环境规则")
}

pub(super) fn view_idle_local<R: IdleDispatch, F>(
    future: F,
) -> RawResult<TaskHandle<R::Notifier, F::Output>, SpawnError<F, R::Notifier>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    todo!("调用 `R::idle_local`，遵守提供方当前本地环境规则")
}

pub(super) fn view_timeout<R: RuntimeTime>(runtime: &R, duration: Duration) -> Timeout<R::Timer> {
    todo!("调用 `R::timeout` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_timeout_on<R: RuntimeTime, F: Future>(
    runtime: &R,
    future: F,
    duration: Duration,
) -> TimeoutOn<R::Timer, F> {
    todo!("调用 `R::timeout_on` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_sleep<R: RuntimeTime>(runtime: &R, duration: Duration) -> Sleep<R::Timer> {
    todo!("调用 `R::sleep` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_wait_one_timeout<R: RuntimeWait, T>(
    runtime: &R,
    duration: Duration,
) -> (WaitOneTimeout<R::Timer, T>, OneSender<T>) {
    todo!("调用 `R::wait_one_timeout` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_wait_any_timeout<R: RuntimeWait, T>(
    runtime: &R,
    duration: Duration,
) -> (WaitAnyTimeout<R::Timer, T>, AnySender<T>) {
    todo!("调用 `R::wait_any_timeout` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_wait_all_timeout<R: RuntimeWait, T>(
    runtime: &R,
    duration: Duration,
) -> (WaitAllTimeout<R::Timer, T>, AllSender<T>) {
    todo!("调用 `R::wait_all_timeout` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_id<R: RuntimeQuery>(runtime: &R) -> RuntimeId {
    todo!("调用 `R::id` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_status<R: RuntimeQuery>(runtime: &R) -> RuntimeStatus {
    todo!("调用 `R::status` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_is_driving<R: RuntimeQuery>(runtime: &R) -> bool {
    todo!("调用 `R::is_driving` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_last_idle_state<R: RuntimeQuery>(runtime: &R) -> IdleState {
    todo!("调用 `R::last_idle_state` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_close_state<R: RuntimeQuery>(runtime: &R) -> CloseState {
    todo!("调用 `R::close_state` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_external_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::external_task_count` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_internal_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::internal_task_count` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_timer_registration_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::timer_registration_count` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_idle_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::idle_task_count` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_in_flight_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::in_flight_task_count` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_waiting_for_wake_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::waiting_for_wake_count` 一次，按其原生契约交付参数与结果")
}

pub(super) fn view_pending_cancel_task_count<R: RuntimeQuery>(runtime: &R) -> usize {
    todo!("调用 `R::pending_cancel_task_count` 一次，按其原生契约交付参数与结果")
}
