//! 必传的配置（Config）输入。宿主（host）交出一份原生配置，插件（plugin）
//! 取用后按 `pi_config` 的 Rust 接口（API）使用，并负责该份配置的清理。
//!
//! 两侧须链接并装入同一次配套构建的 `pi_ffi_adapters` Rust 动态库（dylib），
//! 使用配套的 `pi_config`、Rust 工具链（toolchain）、目标（target）、特性
//! （feature）及共享依赖（dependency）。首批验证使用 crates.io `0.7.0`。
//! 配套构建须保证配置的内存分配和释放（allocation and deallocation）匹配。
//! 宿主管理流程在交接前核对实际加载的库。C 输入使用共同对象表示。

/* 内部设计

发布与保管。宿主管理流程在同一轮次（initialization round）绑定所有输入。
publish 用 Connection 附着 B1 来源，再登记 PendingConfig 中的一份原生 Config。
来源为实际对象及首持有建立持续记录。成功后用 PublishedConfig 保持原持有。
若附着失败，登记前的配置由 publish 在锁外清理。若 B1 已接管配置，归还在
B1 的持续记录中推进。PendingConfig 的关闭只确认可以清理该条记录，配置如果
仍在槽中，由 B1 在锁外按原 Rust 规则销毁。对外发布后，登记槽是待取配置的唯一拥有方。
匹配的分配器代码和上下文须持续到最后一次释放。已有配置回调的登记方保证其
代码和上下文在回调和析构完成前有效，包括注销前已派发的回调。

借出。with_input 用 Object::with_borrowed 取得读取保护。把原 ObjectRefV1 的
地址与同一实际 Registry 暂存到当前线程栈顶对应的 TLS 记录。嵌套时保留上一记录；
作用域退出时先恢复它，再结束本次 B1 借用。恢复守卫直接用已借用的槽位，
TLS 不可用报 InvalidState。结束短期 RefCell 借用后才进入闭包、销毁替换的记录
或归还 B1 持有。须在借用范围内保存标准初始化结果、诊断和已经发生的操作。

接收。标准入口先核全部必传入参，再由 ConfigInput::from_input 核对其收到的
同步借用地址恰是本线程最内层借出的配置。无匹配报 Conflict，TLS 不可用报
InvalidState。此检查不解引用登记的地址，通过后才按字段重建 Connection 引导
所需的临时值，沿 B1 建立连接，取得独立持有并查询准确的只读 ConfigInterface 表头。
重建值仍借用同一原持有。连接有自己的 B1 独立持有；用于取用的接口持有仅在
ConfigInput 存活时保留。构造失败由已有的责任记录处理实际已取得的持有。
B5 在 take 前核对终端及已提供的运行时属于本轮，并取得相应 Rust 输入。
这些输入的核对不消费配置（Config）。初始化数据（initialization data）D 的预核与
取用由上层先完成，随后才取配置（Config）。配置收到与否及内容是否合适是两个问题，空配置可以交接。

取用。ConfigInput 借用真实输入，限当前调用和线程。take 消费壳，经 Interface
取得代码保护，来源先核同一借用地址仍在最内层，再用 Registry::with_access 对
实际 PendingConfig 取得本次 TAKE 访问。该槽的短期 Mutex 只保护 Option::take。
槽由有值变为空是取出时点。释放互斥锁后处理结果，空槽报 InvalidState。本轮只
取一次。插件代码、配置接口和析构均在锁外。访问与保活归还完成后以 Rust 原值
交付。归还若在取出之后报错，本次以 pi_result 失败退出，
已取的值由接收侧在锁外销毁，槽保持为空。B5 保存已发生的调用结果与清理责任；
同轮重试复用已记录结果。若还未取用，配置仍归来源登记表；新一轮自行提供新值。
正常 Rust 栈展开依 B1 归还保护，标准 C 入口在各自约定的 Rust 边界处理恐慌。

使用。成功后返回的 Config 由插件独占，可在 Rust 代码中移动，按原库借出
&mut Config。配置字段、路径、缓存与来源按原库接口使用。插件按标准关闭合同
清理配置和其自行登记的回调，包括已派发的回调。对于使用 pi_config 0.6 的库，
包装代码按原方式单独构造旧版配置。交接期间的 pi-ffi 失败一律使用 pi_result。
业务配置错误由插件增加业务上下文后按标准结果报告。
*/

#[path = "config_inner.rs"]
mod inner;

use parking_lot::Mutex;
use pi_ffi_abi::{ApiHeaderV1, InterfaceRequestV1, LeaseV1, ObjectRefV1, UuidV1};
use pi_ffi_core::{
    control::Connection,
    object::{AbiInterface, Interface, Object},
    source::Registry,
};
use pi_result::{Error, ErrorKind, Result};
use std::{cell::RefCell, sync::Arc};

pub use pi_config::{self, Config};

struct ConfigInterface;

#[repr(C)]
struct ConfigTable {
    header: ApiHeaderV1,
}

const READ: u64 = 1;
const TAKE: u64 = 2;

// 安全性（safety）。ConfigTable 只有固定 C 表头。借出使用 B1 读取权限
// （permission）。取用使用 B1 发起操作的权限，实际值由同一份 Rust 适配库处理。
unsafe impl AbiInterface for ConfigInterface {
    type Table = ConfigTable;
    const ID: UuidV1 = UuidV1 {
        bytes: [
            0xe0, 0xae, 0x98, 0xf0, 0xb3, 0x0c, 0x4f, 0x3b, 0x91, 0x3a, 0x7b, 0x61, 0xc8, 0x69,
            0x4e, 0x79,
        ],
    };
    const MAJOR: u32 = 1;
    const SUPPORTED_RIGHTS: u64 = READ | TAKE;
}

const CONFIG_REQUEST: InterfaceRequestV1 = InterfaceRequestV1 {
    interface_id: ConfigInterface::ID,
    major: ConfigInterface::MAJOR,
    size: std::mem::size_of::<ConfigTable>() as u32,
};

static CONFIG_TABLE: ConfigTable = ConfigTable {
    header: ApiHeaderV1 {
        major: CONFIG_REQUEST.major,
        size: CONFIG_REQUEST.size,
    },
};

struct PendingConfig {
    value: Mutex<Option<Config>>,
}

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

/// 宿主（host）已登记的一份配置（Config），绑定到一次初始化轮次。
pub struct PublishedConfig {
    object: Object,
    source: Arc<Registry>,
}

impl PublishedConfig {
    /// 登记本轮交给插件（plugin）的一份原生配置（Config）。
    ///
    /// `connection` 必须是宿主管理流程为本轮建立的连接。调用接管 `config`。
    /// 发生错误（error）时，依据已经取得的持有（holding）由本侧或共同释放记录
    /// 清理。
    ///
    /// # Safety
    ///
    /// 调用方已核对参与本次交接的 Rust 库及分配器（allocator）。配置携带的内存
    /// 可由适配库和接收方按其分配方式正确释放，分配器以及配置携带的外部代码和
    /// 上下文持续有效至最后一次使用及清理。宿主管理流程负责建立这些前提。
    // @trace 1
    pub unsafe fn publish(connection: Connection, config: Config) -> Result<Self> {
        inner::publish(connection, config)
    }

    /// 在一次同步初始化调用中借出配置（Config）的 C 输入。
    ///
    /// 调用方持有本轮初始化资格，并保护插件代码。在闭包（closure）内保存标准
    /// 调用结果、诊断和已经发生的操作。闭包至多执行一次，正常返回和栈展开
    /// （stack unwinding）都结束借用。保护归还失败按调用实际进度处理。
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

/// 插件（plugin）在当前同步初始化中收到的配置（Config）输入。
///
/// 用它取得其它输入所需的共同连接（connection），再取用该轮的配置。
pub struct ConfigInput<'call> {
    input: &'call ObjectRefV1,
    connection: Connection,
    interface: Interface<ConfigInterface>,
    source: Arc<Registry>,
}

impl<'call> ConfigInput<'call> {
    /// 从收到的配置（Config）输入建立本轮共同连接（connection）。
    ///
    /// # Safety
    ///
    /// `input` 必须借自当前同步初始化收到的原始地址。该借用实际保有合法
    /// 持有（holding）、来源代码、上下文及诊断表。两侧已经完成 Rust 配套
    /// 构建（build）、加载核对（load verification）和内存分配／释放匹配确认。
    /// 入口检查当前线程及地址，按 B1 获取连接和独立持有。
    // @trace 3
    pub unsafe fn from_input(input: &'call ObjectRefV1) -> Result<Self> {
        let (source, seed) = inner::bootstrap_input(input)?;
        // 安全性（safety）。seed 重建自本次已受保护且匹配的原输入。
        let connection = unsafe { Connection::from_borrowed(seed) }?;
        // 安全性（safety）。仍在原输入借用期内，且连接已由此输入引导。
        let object = unsafe { Object::retain_borrowed(&connection, input) }?;
        let interface = object.interface::<ConfigInterface>()?;
        Ok(Self {
            input,
            connection,
            interface,
            source,
        })
    }

    /// 借用当前共同连接（connection），用于建立同一轮的其余输入。
    // @trace 4
    pub fn connection(&self) -> &Connection {
        inner::connection(self)
    }

    /// 在终端（terminal）、已提供的运行时（runtime）及数据（data）预核后，
    /// 由上层先接收数据（data），再取出这一轮的配置（Config）。
    ///
    /// 成功后由插件（plugin）持有原生配置。取出后若保护归还失败，则报告
    /// `pi_result`，接收侧清理该份配置，该轮继续按已发生的取用处理。
    /// 轮次或输入不符、再次取用等错误报告相应的原因。
    // @trace 5
    pub fn take(self) -> Result<Config> {
        self.interface.with_call(TAKE, |call| inner::take(&self, call.lease()))
    }
}

impl Drop for RestoreInput<'_> {
    // @trace 6
    fn drop(&mut self) {
        inner::restore_input(self)
    }
}
