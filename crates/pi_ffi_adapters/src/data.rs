//! 按值交接初始化数据（initialization data）。具体类型由宿主（host）与插件
//! （plugin）约定，只要求 `Send + Sync + 'static`，不要求 `Clone`。
//!
//! 对通过类型预核的 D，两侧必须使用同一实际定义与配套 Rust 产物，包括工具链、目标、特性、
//! 依赖和分配器（allocator）；同一次实际加载的适配库保护同步借用。类型身份检查不能证明
//! 任意 Rust 产物的内存布局兼容。数据携带的析构和回调代码须持续有效至最后使用。
//!
//! 调用均为同步（synchronous），共同管理和来源登记可能等待内部锁（lock）；
//! 不持锁执行用户析构（Drop）、回调（callback）或诊断（diagnostic）。
//!
//! 缺少线程转移（Send）能力的值不能发布，即使它支持共享访问（Sync）：
//! ```compile_fail
//! use pi_ffi_adapters::data::PublishedData;
//! use pi_ffi_core::control::Connection;
//! fn rejected(connection: Connection, data: std::sync::MutexGuard<'static, ()>) {
//!     unsafe { PublishedData::publish(connection, data) }.unwrap();
//! }
//! ```
//!
//! 缺少共享访问（Sync）能力的值也不能发布：
//! ```compile_fail
//! use pi_ffi_adapters::data::PublishedData;
//! use pi_ffi_core::control::Connection;
//! fn rejected(connection: Connection) {
//!     unsafe { PublishedData::publish(connection, std::cell::Cell::new(1u8)) }.unwrap();
//! }
//! ```
//!
//! 输入不能携带期限不足的借用（borrowing）：
//! ```compile_fail
//! use pi_ffi_adapters::data::PublishedData;
//! use pi_ffi_core::control::Connection;
//! fn rejected<'a>(connection: Connection, data: &'a [u8]) {
//!     unsafe { PublishedData::publish(connection, data) }.unwrap();
//! }
//! ```

/* 内部设计

发布（publication）。附着本轮来源（source）并登记 PendingData<D>。发布前失败在锁
（lock）外销毁输入；接管之后沿共同持有（holding）记录清理。唯一新增互斥锁
（Mutex）只保护 Option::take，不执行外部代码。

借出（borrowing）。Object::with_borrowed 保护原持有（holding），在本线程（thread）
最内层记录原地址与实际来源（source）。RestoreInput 在正常返回和栈展开
（stack unwinding）时恢复上一层；RefCell 借用结束后才调用闭包（closure）或析构（Drop）。

接收（receiving）。检查线程（thread）和原地址、来源（source）身份、共同连接
（connection）的真实权威（authority）及轮次（round）；取得独立持有（holding），
查询准确表头（header）。用 READ 访问真实 PendingData<D> 预核类型，不取值。
所有初始化（initialization）输入预核完成后，上层先记录可能消费进度，再调用
take_into，最后取配置（Config）并交付。

取用（taking）。目标必须为空。通过 TAKE 调用保护（call protection），再核最内层
来源（source）、实际类型与权限（permission），在短锁（lock）内取空后把原 D 写到
外部 Option，闭包（closure）只返回 ()。归还保护即使失败，输出仍保存已经取出的 D。
空槽返回 InvalidState。重复结果不重新进入此路径。

清理（cleanup）。交给插件（plugin）前，上层在独立边界（boundary）内显式销毁
外部 Option 中的 D；清理失败先固定原有 CleanupFailed 和失败快照（snapshot），再
处理诊断（diagnostic）。交付以后由插件（plugin）定义其内部语义。
本模块（module）不负责关闭宿主运行时（runtime）或终端（terminal）。
*/

#[path = "data_inner.rs"]
mod inner;

use parking_lot::Mutex;
use pi_ffi_abi::{ApiHeaderV1, InterfaceRequestV1, LeaseV1, ObjectRefV1, UuidV1};
use pi_ffi_core::{
    control::Connection,
    object::{AbiInterface, Interface, Object},
    source::Registry,
};
use pi_result::{Error, ErrorKind, Result};
use std::{cell::RefCell, marker::PhantomData, sync::Arc};

struct DataInterface;

#[repr(C)]
struct DataTable {
    header: ApiHeaderV1,
}

const READ: u64 = 1;
const TAKE: u64 = 2;

// SAFETY: 表只含固定 C 表头；具体 D 由来源的真实 TypeId 与 Any 核对后访问。
unsafe impl AbiInterface for DataInterface {
    type Table = DataTable;
    const ID: UuidV1 = UuidV1 {
        bytes: [
            0x42, 0xff, 0x65, 0x5d, 0xbd, 0x6b, 0x49, 0x80, 0x93, 0xc7, 0xe7, 0x2e, 0x9e, 0xdc,
            0xff, 0xd0,
        ],
    };
    const MAJOR: u32 = 1;
    const SUPPORTED_RIGHTS: u64 = READ | TAKE;
}

const DATA_REQUEST: InterfaceRequestV1 = InterfaceRequestV1 {
    interface_id: DataInterface::ID,
    major: DataInterface::MAJOR,
    size: std::mem::size_of::<DataTable>() as u32,
};

static DATA_TABLE: DataTable = DataTable {
    header: ApiHeaderV1 {
        major: DATA_REQUEST.major,
        size: DATA_REQUEST.size,
    },
};

struct PendingData<D: Send + Sync + 'static> {
    // 每份数据一把短锁，仅保护唯一移动；所有析构均在锁外。
    value: Mutex<Option<D>>,
}

struct InitializationInput {
    address: *const ObjectRefV1,
    source: Arc<Registry>,
}

thread_local! {
    static PI_FFI_ADAPTERS_DATA_INPUT: RefCell<Option<InitializationInput>> = const { RefCell::new(None) };
}

struct RestoreInput<'scope> {
    slot: &'scope RefCell<Option<InitializationInput>>,
    previous: Option<InitializationInput>,
}

/// 宿主（host）在本轮登记的初始化数据（initialization data），最多交出一次。
pub struct PublishedData {
    object: Object,
    source: Arc<Registry>,
}

impl PublishedData {
    /// 接管原 `data`，成功后由共同持有（holding）记录保管；失败不承诺退回原值。
    ///
    /// # Safety
    ///
    /// `connection` 是本轮合法连接。双方已核配套 Rust 构建（matching build）及
    /// 通过类型预核后实际解释的 D 定义，使用匹配的分配器（allocator）。
    /// D 携带的内存、析构、回调及相关代码在最后使用和清理前持续有效。
    /// `Send + Sync + 'static` 和类型身份相等不能替代这些前提。
    // @trace 1
    pub unsafe fn publish<D: Send + Sync + 'static>(
        connection: Connection,
        data: D,
    ) -> Result<Self> {
        inner::publish(connection, data)
    }

    /// 在同步初始化调用中借出原始 C 输入。闭包（closure）至多执行一次。
    ///
    /// 调用方保护本轮资格和插件代码，并在闭包内保存标准结果、诊断及实际操作。
    /// 正常返回和栈展开均恢复上一层借用；保护归还失败不回滚已经取出的值。
    // @trace 2
    pub fn with_input(
        &self,
        invoke: impl for<'call> FnOnce(&'call ObjectRefV1) -> Result<()>,
    ) -> Result<()> {
        self.object.with_borrowed(|input| {
            PI_FFI_ADAPTERS_DATA_INPUT
                .try_with(|slot| {
                    let _restore = inner::install_input(self, input, slot);
                    invoke(input)
                })
                .map_err(|_| Error::new(ErrorKind::InvalidState))?
        })
    }
}

/// 插件（plugin）在当前同步调用中收到的数据输入，绑定实际类型 D。
pub struct DataInput<'call, D: Send + Sync + 'static> {
    input: &'call ObjectRefV1,
    interface: Interface<DataInterface>,
    source: Arc<Registry>,
    marker: PhantomData<fn(D) -> D>,
}

impl<'call, D: Send + Sync + 'static> DataInput<'call, D> {
    /// 预核本轮来源、地址、权限和实际类型，成功不消费原值。
    ///
    /// # Safety
    ///
    /// `input` 是当前同步借出的原始输入地址，真实持有、来源代码和上下文仍受
    /// 保护。`connection` 从同一次初始化的配置输入建立。双方满足
    /// [`PublishedData::publish`] 的类型、构建、分配器及代码存续前提。
    /// 不要求预先证明登记类型就是 D；实际类型不匹配返回 `Conflict`，不消费原值。
    // @trace 3
    pub unsafe fn from_input(connection: &Connection, input: &'call ObjectRefV1) -> Result<Self> {
        // SAFETY: 调用方保护原始输入及本轮连接，并已确认配套类型与代码。
        unsafe { inner::receive(connection, input) }
    }

    /// 将唯一原值移入空的 `output`，不要求 `Clone`。
    /// 内部同步锁（synchronous lock）可能等待；不持锁执行析构（Drop）或外部回调（callback）。
    ///
    /// 目标非空或原值已取走返回 `InvalidState`，不覆盖目标。不兼容的来源、
    /// 权限、类型或借用返回对应错误。即使返回错误，`output` 仍可能已取得原值；
    /// 调用方须按实际输出负责后续交付或受保护清理，不按错误推定未消费。
    // @trace 4
    pub fn take_into(self, output: &mut Option<D>) -> Result<()> {
        if output.is_some() {
            return Err(Error::new(ErrorKind::InvalidState));
        }
        self.interface
            .with_call(TAKE, |call| inner::take_into(&self, call.lease(), output))
    }
}

impl Drop for RestoreInput<'_> {
    // @trace 5
    fn drop(&mut self) {
        inner::restore_input(self)
    }
}
