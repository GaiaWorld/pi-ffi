// 占位（stub）尚未消费参数。
#![allow(unused_variables, dead_code)]

use super::*;

pub(super) fn publish<T>(connection: Connection, terminal: Arc<T>) -> Result<PublishedTerminal>
where
    T: Send + Sync + 'static,
{
    todo!("附着 B1 来源，登记同一 Arc<T>、只读表头及本条关闭回调；成功保存原持有，失败按已经取得的责任归还")
}

pub(super) fn install_input<'scope>(
    published: &PublishedTerminal,
    input: &ObjectRefV1,
    slot: &'scope RefCell<Option<InitializationInput>>,
) -> RestoreInput<'scope> {
    todo!("在本线程记录原输入地址和实际 Registry，上一条记录交给恢复守卫，归还短期可变借用")
}

pub(super) fn source_for_input(connection: &Connection, input: &ObjectRefV1) -> Result<Arc<Registry>> {
    todo!("只核当前线程最内层原地址和同轮次，线程局部存储不可用报 InvalidState，不匹配报 Conflict；返回同一来源的共享持有")
}

pub(super) fn native<T>(terminal: &Terminal<T>) -> &T {
    todo!("借用当前插件已取得的原生终端，不移动 Arc 及 B1 持有")
}

pub(super) fn clone_terminal<T>(terminal: &Terminal<T>) -> Terminal<T> {
    todo!("克隆同一 Arc<T> 并共享现有 B1 持有，保证先归还终端再归还代码保护")
}

pub(super) fn restore_input(restore: &mut RestoreInput<'_>) {
    todo!("从守卫已有槽位恢复上一记录，结束短期可变借用后再释放换出的记录")
}

pub(super) fn register_static<T, D>(
    terminal: &T,
    declarations: impl IntoIterator<Item = D>,
    register: impl FnOnce(&T, Vec<D>) -> Result<()>,
) -> Result<()> {
    todo!("在调用框架前收完整集合并以 pi_result 报分配失败；空集合成功；非空只调用一次实际框架登记并原样返回")
}
