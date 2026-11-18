// 占位（stub）尚未消费参数。
#![allow(unused_variables, dead_code)]

use super::*;

pub(super) fn publish(connection: Connection, config: Config) -> Result<PublishedConfig> {
    todo!("附着来源，登记唯一 PendingConfig 与只读表头，按 B1 接管原持有；登记前失败与已登记失败分别按实际归属清理")
}

pub(super) fn install_input<'scope>(
    published: &PublishedConfig,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    todo!("在最内层保存当前借用地址及同一 Registry，把上一记录交给恢复守卫；结束短期可变借用后返回")
}

pub(super) fn bootstrap_input(input: &ObjectRefV1) -> Result<(Arc<Registry>, ObjectRefV1)> {
    todo!("只核本线程最内层记录，槽位不可用报 InvalidState，地址不符报 Conflict；取得来源并按字段重建同一借用供引导")
}

pub(super) fn connection<'access>(config: &'access ConfigInput<'_>) -> &'access Connection {
    todo!("借用同一轮的 Connection")
}

pub(super) fn take(config: &ConfigInput<'_>, lease: LeaseV1) -> Result<Config> {
    todo!("核最内层配置借用仍匹配；在来源校验 PendingConfig 与 TAKE 后锁内取一次原值，空槽报 InvalidState；锁外归还访问和处理原值")
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    todo!("利用守卫借用的槽位恢复原记录，结束短期可变借用后丢弃本轮记录")
}
