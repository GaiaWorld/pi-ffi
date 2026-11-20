//! 必传的 P2P 终端（P2P service terminal）及静态服务（static service）汇总。
//!
//! 宿主（host）提供已构造的原生终端。插件（plugin）在 Rust 中使用同一个
//! 实例。双方按同一次配套构建链接实际相同的适配库（adapter crate），匹配
//! 终端、编解码器（codec）、附件处理类型及其 Rust 依赖。配套构建及加载
//! 核对包括原生引用使用的分配器（allocator）和代码存续。

/* 内部设计

发布。B5 核对配套制品和本轮输入后，以 Connection 附着来源。来源经 Registry
登记宿主实际 Arc<T> 与固定只读表头。T 为真正的 P2pServiceTerminal<C, S>，宿主
和插件使用匹配的原生 Rust 类型。适配模块以一般 Rust 类型持有 T，真实实例和
默认／自定义 Codec 与附件类型由两侧配套构建确定。这样适配库的链接关系不会替
pi_service 选择其静态收集表的归属。登记前失败由当前调用归还 Arc，登记已接管
时走 B1 记录。成功先保存原持有再发布。来源的关闭回调只确认本条可清理，B1
在锁外归还来源的实际 Arc。外部宿主拥有终端的管理权。

借出。Object::with_borrowed 取得本次读取保护。把原借用地址和实际 Registry
放入本线程最内层记录。嵌套时把上一条交给当前栈上的恢复守卫。结束短期可变
借用后执行标准初始化闭包，闭包在保护内接管结果、诊断和实际进度。退出先
恢复上一条再归还借用。保护归还错误按 B1 传播。Rust 栈展开使用同一恢复和
归还路径，约定捕获恐慌的 C 入口在各自 Rust 制品内捕获。

接收。配置已建立共同连接。匹配当前线程最内层的原输入地址后检查轮次，
沿 Object::retain_borrowed 核实际共同权威并取得独立持有，查询精确表头。
借用地址只用于相等比较。随后在 Interface::with_call 的读取调用保护及
Registry::with_access 实际访问内，唯一一次读取已登记的 Arc<T> 并取得
接收侧的 Rust 共享持有。取用前读取权限和真实类型都由来源核对。
保护归还失败时，在保护涵盖的配套环境中归还刚取得的共享持有并报错。
全部成功后，Terminal 保留这一份 Arc<T> 与对应 B1 持有，字段顺序保证
先销毁本地 Arc，再归还 B1 持有。取得失败的独立持有沿 B1 已登记责任归还。

使用。Terminal::native 借用当前 Rust 持有的原生终端。引用只随本地 Terminal
存活，插件可依原 Rust 接口跨 await 使用。原生调用使用插件已经接收的一份
Rust 共享持有。取得和归还受控 C 对象沿 B1；后续使用遵守原生终端的合同。
本地 Clone 共享本次已接收的 B1 持有并克隆同一 Arc<T>，无新增登记。
插件从原生接口得到可独立持有的句柄或回调时，须保留对应的 Terminal 以保持本轮
持有。服务（services）的实现、注册（registration）、管理和注销（unregistration）
完全属于插件（plugin）内部。标准关闭（close）的实际返回由插件（plugin）决定，
本库不检查内部清单或状态（states）。归还 Terminal 前仍须满足派生持有与引用的
存续条件；终端（terminal）管理权和其他使用者归外部拥有方。

可选的静态声明（static declarations）汇总。插件（plugin）可在自己的 Rust
代码中调用 register_static，也可直接使用原生接口（native APIs）。采用本函数时，
传入插件（plugin）自身的 inventory 迭代项（iterator items）或构建时确定的明确清单。实际装配仍须
保证收集表、节点、分配器（allocator）和相关代码的存续；采用明确清单不能抵消
加载构造器（loading constructor）向共享表提交节点的实际效果。B5 核对的是配套
构建（matching build），不枚举插件（plugin）的业务清单或内部状态（states）。
本函数只收集一次：空集合（empty collection）成功；非空集合（nonempty collection）交给插件（plugin）提供的 register 闭包
（closure）一次，并原样传播其 pi_result。收集时分配失败（allocation failure）发生在闭包（closure）
之前。本函数不初始化（initialize）服务（services），也不确认业务就绪（business readiness）或保证整批效果。
服务（services）的实际管理及失败进度由插件（plugin）自行负责，对外完全透明。
静态服务（static services）声明并注册（registration）后在当前 P2P 主机网络（P2P host network）
中不注销（unregistration），可暂停或暂时停止。动态服务（dynamic services）可在运行时（runtime）
注销（unregistration）。本库不要求额外的登记（registration）、撤回（withdrawal）或完成接口（APIs）；宿主（host）管理流程仅使用
标准 C 接口（C APIs）的实际结果及本库自身 B1 持有与调用事实。
*/

#[path = "service_inner.rs"]
mod inner;

use pi_ffi_abi::{ApiHeaderV1, InterfaceRequestV1, ObjectRefV1, UuidV1};
use pi_ffi_core::{
    control::Connection,
    object::{AbiInterface, Interface, Object},
    source::Registry,
};
use pi_result::{Error, ErrorKind, Result};
use std::{cell::RefCell, sync::Arc};

struct TerminalInterface;

#[repr(C)]
struct TerminalTable {
    header: ApiHeaderV1,
}

const READ: u64 = 1;

// 安全性（safety）。表仅含固定 C 表头，来源只在已核实登记与权限后提供
// 一份 Rust 共享持有。取用真实终端依赖匹配的原生类型及存续保证。
unsafe impl AbiInterface for TerminalInterface {
    type Table = TerminalTable;
    const ID: UuidV1 = UuidV1 {
        bytes: [
            0x64, 0x31, 0x7c, 0x04, 0x30, 0xf5, 0x45, 0xdb, 0xa2, 0xce, 0xa6, 0x85, 0xe3, 0x87,
            0x59, 0xee,
        ],
    };
    const MAJOR: u32 = 1;
    const SUPPORTED_RIGHTS: u64 = READ;
}

const TERMINAL_REQUEST: InterfaceRequestV1 = InterfaceRequestV1 {
    interface_id: TerminalInterface::ID,
    major: TerminalInterface::MAJOR,
    size: std::mem::size_of::<TerminalTable>() as u32,
};

static TERMINAL_TABLE: TerminalTable = TerminalTable {
    header: ApiHeaderV1 {
        major: TERMINAL_REQUEST.major,
        size: TERMINAL_REQUEST.size,
    },
};

struct InitializationInput {
    address: *const ObjectRefV1,
    source: Arc<Registry>,
}

thread_local! {
    static CURRENT_INPUT: RefCell<Option<InitializationInput>> = const { RefCell::new(None) };
}

struct RestoreInput<'scope> {
    slot: &'scope RefCell<Option<InitializationInput>>,
    previous: Option<InitializationInput>,
}

/// 宿主（host）登记的必传 P2P 终端（P2P service terminal）。
pub struct PublishedTerminal {
    object: Object,
    source: Arc<Registry>,
}

impl PublishedTerminal {
    /// 登记宿主（host）提供的一份实际终端（terminal）共享持有。
    ///
    /// # Safety
    ///
    /// `T` 是实际、配套构建的 `pi_p2p_service::P2pServiceTerminal<C, S>`。
    /// 宿主管理流程已取得本轮准入并核实终端的 Rust 依赖、分配器和代码存续。
    /// 终端和它包含的外部代码可在双方合法使用并在最后一次拥有期间正常释放。
    // @trace 1
    pub unsafe fn publish<T>(connection: Connection, terminal: Arc<T>) -> Result<Self>
    where
        T: Send + Sync + 'static,
    {
        inner::publish(connection, terminal)
    }

    /// 在一次同步标准初始化中借出终端（terminal）的 C 输入。
    ///
    /// 期间调用方保护本轮及插件（plugin）代码，并在闭包（closure）内接管
    /// 实际结果和诊断。闭包至多执行一次。保护归还失败按实际进度处理。
    // @trace 2
    pub fn with_input(
        &self,
        invoke: impl for<'call> FnOnce(&'call ObjectRefV1) -> Result<()>,
    ) -> Result<()> {
        self.object.with_borrowed(|input| {
            CURRENT_INPUT
                .try_with(|slot| {
                    let _restore = inner::install_input(self, input, slot);
                    invoke(input)
                })
                .map_err(|_| Error::new(ErrorKind::InvalidState))?
        })
    }
}

/// 插件（plugin）持有的原生终端（terminal）。
///
/// 按字段顺序先归还本地 Rust 共享持有，再归还对应 B1 持有。插件可以在
/// 自己的 Rust 代码中使用终端。还持有从终端取得的原生句柄（handle）、
/// 引用或回调（callback）时，须保留对应的 `Terminal`。归还终端（terminal）
/// 持有前须满足这些派生对象的存续条件。服务（services）管理由插件（plugin）
/// 内部负责；本库不要求静态注销（static unregistration）。
pub struct Terminal<T> {
    terminal: Arc<T>,
    holding: Interface<TerminalInterface>,
}

impl<T> Terminal<T>
where
    T: Send + Sync + 'static,
{
    /// 从本次同步初始化的必传输入接收一份原生终端（terminal）持有。
    ///
    /// # Safety
    ///
    /// `input` 借自当前调用收到的原始终端输入。借用实际覆盖来源代码、
    /// 上下文及表。配置（Config）已经建立本轮的合法 `connection`。
    /// 两侧已核实实际装入的适配库、原生类型、分配器及对应代码存续。
    /// 接收方保证从原生终端派生的引用和回调受本轮持有保护，完成清理才释放。
    // @trace 3
    pub unsafe fn from_input(connection: &Connection, input: &ObjectRefV1) -> Result<Self> {
        let source = inner::source_for_input(connection, input)?;
        // 安全性（safety）。输入仍在实际同步借用期，同轮连接已核实。
        let object = unsafe { Object::retain_borrowed(connection, input) }?;
        let holding = object.interface::<TerminalInterface>()?;
        let terminal = holding.with_call(READ, |call| {
            source.with_access::<Arc<T>, Arc<T>>(call.lease(), TERMINAL_REQUEST, READ, |terminal| {
                Ok(Arc::clone(terminal))
            })
        })?;
        Ok(Self { terminal, holding })
    }

    /// 借用原生 P2P 终端（P2P service terminal），按其 Rust 接口（API）使用。
    ///
    /// 本地借用保护当前拥有者，可以在其生命周期中继续使用异步原生调用。
    /// 原生接口交出的独立句柄仍按本轮终端持有与插件（plugin）关闭合同使用。
    // @trace 4
    pub fn native(&self) -> &T {
        inner::native(self)
    }
}

impl<T> Clone for Terminal<T> {
    // @trace 5
    fn clone(&self) -> Self {
        inner::clone_terminal(self)
    }
}

impl Drop for RestoreInput<'_> {
    // @trace 6
    fn drop(&mut self) {
        inner::restore_input(self)
    }
}

/// 可选地汇总插件（plugin）的静态声明（static declarations），调用一次登记（registration）闭包（closure）。
///
/// 插件（plugin）可在自己的 Rust 代码中使用本函数，也可直接使用原生接口（native APIs）。
/// 传入自身收集表的迭代项（iterator items）或构建时确定的明确清单，并满足该装配的归属和存续条件。
/// 空集合（empty collection）直接成功；非空集合（nonempty collection）只调用一次 `register`，其实际结果原样返回。
/// 收集时分配失败（allocation failure）在调用闭包（closure）前返回。迭代（iteration）或闭包（closure）发生的
/// 栈展开（stack unwinding）沿既有边界处理，已发生的实际效果仍由插件（plugin）负责。
/// 本函数不判断服务（services）是否就绪（readiness），不要求静态注销（static unregistration），
/// 不决定标准初始化接口（initialization API）或标准关闭接口（close API）的返回。
// @trace 7
pub fn register_static<T, D>(
    terminal: &Terminal<T>,
    declarations: impl IntoIterator<Item = D>,
    register: impl FnOnce(&T, Vec<D>) -> Result<()>,
) -> Result<()>
where
    T: Send + Sync + 'static,
{
    inner::register_static(terminal.native(), declarations, register)
}
