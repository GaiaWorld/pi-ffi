//! 宿主（host）以 Rust 接口查找、加载、初始化、关闭及卸载插件。

#[path = "host_inner.rs"]
mod inner;

use crate::{
    authority::Authority,
    config::Config,
    runtime::{IdleDispatch, RuntimeQuery, RuntimeWait},
};
use pi_ffi_abi::InitIdV1;
use pi_result::Result;
use std::sync::Arc;

pub use semver::{Version, VersionReq};

pub use crate::package::{
    BuildDependency, BuildFile, BuildManifest, DependencyUse, HostBuild, Metadata, PluginPackage,
    PluginRequest, StaticDeclarations, Target,
};

/// 宿主选定的真实 P2P 终端实例（terminal instance）。
pub struct TerminalInput<T> {
    terminal: Arc<T>,
}

/// 同一宿主进程（host process）中的管理入口。
#[derive(Clone, Copy)]
pub struct Manager {
    authority: &'static Authority,
}

/// 一次实际模块（module）的本地管理入口。
#[derive(Clone)]
pub struct Plugin {
    authority: &'static Authority,
    epoch: u64,
    metadata: Arc<Metadata>,
}

/// 已登记的初始化轮次（initialization round）。失败后仍可用同一入口关闭。
#[derive(Clone)]
pub struct Initialization {
    authority: &'static Authority,
    epoch: u64,
    round: u64,
}

/*
进程权威在外部加载和插件调用前先建立恢复记录。协议检查后取得代码调用
资格，解除内部锁，再调用或运行析构。返回后先记录本轮真实进度，在保护内
复制／归还诊断，再退出实际输入借用。二次归还错误不会把已调用变成未调用。

配置在 prepare 移入本轮。重复 initialize 使用原记录及原输入。初始化被明确
拒绝且无插件侧效果时，只归还实际交接的本库对象。结果不明、未完成归还、
或关闭失败时保留轮次与占用。本地 Plugin / Initialization 的克隆不额外加载
或登记新轮。

加载（load）沿原实际映像和入口核对。元信息的版本字节从实际映像安全复制后，
在核候选身份与请求之前通过 SystemOperation::record_metadata 保存。成功发布前
实际身份不得成为新的管理许可。这样装入后拒绝仍保有实际和已批准的关联身份。

不同版本允许实际模块共存。插件用自己的配置与真实框架决定如何准备及发布
服务。初始化成功不作为外部服务已切换的证据，静态登记清理仍由插件负责。
*/

impl<T: Send + Sync + 'static> TerminalInput<T> {
    /// 包装已有实例供标准初始化使用。
    ///
    /// # Safety
    ///
    /// T 是实际配套的 pi_p2p_service::P2pServiceTerminal<C, S>，同一原生类型及
    /// 配套代码持续到最后一次使用和释放。持有真实实例，空的逻辑代理不满足合同。
    // @trace 2
    pub unsafe fn new(terminal: Arc<T>) -> Self {
        unsafe { inner::terminal_input(terminal) }
    }
}

impl Manager {
    /// 获取实际宿主进程的同一管理入口。宿主部署使用一份管理实现。
    // @trace 3
    pub fn process(build: HostBuild) -> Result<Self> {
        inner::manager_process(build)
    }

    /// 在宿主提供的候选中选版本。同一实际文件的别名须有相同卸载选择。
    /// 最高优先级有多个实际候选或选择冲突时返回 `Conflict`。
    // @trace 4
    pub fn find(
        &self,
        packages: &[PluginPackage],
        request: &PluginRequest,
    ) -> Result<PluginPackage> {
        inner::manager_find(self, packages, request)
    }

    /// 装入明确指定的插件包。完成实际入口、身份、版本和卸载选择校验后交付管理入口。
    ///
    /// 返回已保留映像的管理入口前重新核验并分配新管理代（management generation）。
    /// 同一个活动映像使用已存在的管理代。装入或退役正在进行时可返回 `Busy`。
    /// 装入会访问文件及系统加载器（system loader），可能执行制品的构造与元信息入口。
    // @trace 5
    pub fn load(&self, package: &PluginPackage, request: &PluginRequest) -> Result<Plugin> {
        inner::manager_load(self, package, request)
    }

    /// 有界推进已登记的归还或退役，`budget` 限定本次推进项数。
    /// 已完成保留或结果不明的系统引用不进入新的系统释放。
    // @trace 6
    pub fn maintain(&self, budget: usize) -> Result<usize> {
        inner::manager_maintain(self, budget)
    }
}

impl Plugin {

    /// 按初始化 v1 准备本轮，不接收初始化数据（initialization data）D。
    /// 只有启用 init_v1 才可调用；插件没有对应入口时，在预留／发布前返回 Unsupported。
    /// 同轮固定版本，不自动改用 v2；输入移入后失败不承诺退回原值。
    #[cfg(feature = "init_v1")]
    // @trace 15
    pub fn prepare_v1<T, R>(
        &self,
        terminal: TerminalInput<T>,
        config: Config,
        runtime: Option<Arc<R>>,
    ) -> Result<Initialization>
    where
        T: Send + Sync + 'static,
        R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
    {
        inner::plugin_prepare_v1::<T, R>(self, terminal, config, runtime)
    }

    /// 不提供运行时（runtime）的专用初始化 v1 准备方法；没有 D 参数。
    /// 版本选择、占用、持有与失败条件同 prepare_v1。
    #[cfg(feature = "init_v1")]
    // @trace 16
    pub fn prepare_v1_without_runtime<T: Send + Sync + 'static>(
        &self,
        terminal: TerminalInput<T>,
        config: Config,
    ) -> Result<Initialization> {
        inner::plugin_prepare_v1_without_runtime::<T>(self, terminal, config)
    }

    /// 读取宿主自己的副本。初始化、关闭和该管理代退役之后均可读取。
    // @trace 7
    pub fn metadata(&self) -> &Metadata {
        inner::plugin_metadata(self)
    }

    /// 按初始化 v2 为一轮交出配置（Config）、终端（terminal）、
    /// 可选运行时（runtime）和初始化数据（initialization data）D，尚未调用插件（plugin）。
    /// D 按值交出，不要求 `Clone`；共享方式由实际类型决定。失败不承诺退回原值。
    /// 已被占用的完整版本不能另开一轮。
    #[cfg(feature = "init_v2")]
    // @trace 8
    pub fn prepare<T, R, D>(
        &self,
        terminal: TerminalInput<T>,
        config: Config,
        runtime: Option<Arc<R>>,
        data: D,
    ) -> Result<Initialization>
    where
        T: Send + Sync + 'static,
        R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
        D: Send + Sync + 'static,
    {
        inner::plugin_prepare::<T, R, D>(self, terminal, config, runtime, data)
    }

    /// 省略运行时（runtime）的初始化 v2；终端（terminal）、配置
    /// （Config）及数据（data）仍必传。无数据内容时传 `()`，失败不承诺退回原值。
    #[cfg(feature = "init_v2")]
    // @trace 9
    pub fn prepare_without_runtime<T: Send + Sync + 'static, D: Send + Sync + 'static>(
        &self,
        terminal: TerminalInput<T>,
        config: Config,
        data: D,
    ) -> Result<Initialization> {
        inner::plugin_prepare_without_runtime::<T, D>(self, terminal, config, data)
    }

    /// 取本模块仍需处理的初始化轮次，包括初始化未完成而需要关闭的轮次。
    // @trace 10
    pub fn current_initialization(&self) -> Result<Option<Initialization>> {
        inner::plugin_current_initialization(self)
    }

    /// 结束此管理代（management generation），此后需重新加载才可使用新代。
    ///
    /// 先用原 [`Initialization::close`] 结束插件工作并归还输入。仍有轮次、
    /// 在途调用或正在进行的文件与映像核验时返回 `Busy`，可用同一入口继续。
    /// 成功后本库归还该管理代的核验文件。默认将已持有的系统引用保留到进程退出。
    /// 使用明确选择提前卸载的包时，成功表示已归还本库的系统引用，映像仍可能
    /// 因其它持有者而存在。
    ///
    /// 重复调用返回此代的结果。旧初始化入口保留本轮结果。
    /// 实际系统释放可能触发平台析构且阻塞调用线程。释放结果不明时返回错误，
    /// 相关地址停用，重复请求只可取得原诊断。
    // @trace 11
    pub fn unload(&self) -> Result<()> {
        inner::plugin_unload(self)
    }
}

impl Initialization {
    /// 返回当前初始化标识（initialization identifier）。
    // @trace 12
    pub fn id(&self) -> InitIdV1 {
        inner::initialization_id(self)
    }

    /// 调用本轮标准初始化。相同请求的稳定结果可重读，在途时返回 Busy。
    // @trace 13
    pub fn initialize(&self) -> Result<()> {
        inner::initialization_run(self)
    }

    /// 关闭原轮次。尚未初始化时归还已登记输入。还未退出时返回 Busy 并保留进度。
    // @trace 14
    pub fn close(&self) -> Result<()> {
        inner::initialization_close(self)
    }
}
