# pi_ffi_core

## 用途与支持环境（Scope and support）

`pi_ffi_core` 为 Rust 宿主（host）与插件动态库（plugin dynamic library）之间的外部函数接口（FFI）提供受控对象交接、调用保护、错误诊断和来源清理。它供 `pi_ffi`、输入适配及遵守同一 ABI 的类型化适配器使用。普通宿主和插件使用 `pi_ffi` 的接入方法即可。

首批项目目标为 Linux x86_64／GNU 和 Windows x86_64／MSVC。库采用 Rust 2021 版次（edition），已核的构建工具链（toolchain）为 Rust `1.99.0-nightly`（`eff8269f7`）。库级文档、示例编译和原生验证按当前阶段记录。最低支持 Rust 版本（MSRV）尚未确定。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 来源（source） | 拥有真实 Rust 对象和本地类型的登记方。取用由该方按实际持有、类型、权限和轮次核实。 |
| 连接（connection） | 关联同一宿主管理权威和初始化轮次的本地 Rust 封装。其克隆（clone）共享原连接。 |
| 独立持有（independent holding） | 来源与宿主均已登记、由一个接收方负责归还的真实持有。对象的本地克隆共用这一份。 |
| 调用保护（call protection） | 一次对实际接口表和上下文有效的使用资格。调用期结束时须归还。 |
| 诊断副本（diagnostic copy） | 由接收方拥有的分类及三段原诊断文本，读取不依赖原模块仍然装入。 |

## Cargo 功能开关（features）

库清单目前无公开功能开关（features）。直接使用默认配置，因而无额外开关组合。最终 Rust 构建的栈展开（stack unwinding）设置需与本库 C 边界的恐慌处理合同配套。

## 先选择对象用法（Object use）

`object::Object` 代表已接管的来源对象。读取本地 `initialization` 无需访问来源代码。按所需存续方式选择方法。

| 方法或类型 | 何时使用 |
| --- | --- |
| `Object::clone` | 在 Rust 本地共享同一份持有，最后本地拥有者负责提交已登记的释放。 |
| `Object::with_borrowed` | 同步借出当前 `ObjectRefV1`。在闭包返回或栈展开时结束借用，闭包保留的下游结果应由本地拥有。 |
| `Object::try_retain` | 新建真实独立持有，原对象照常使用。 |
| `Object::retain_for_transfer`、`TransferObject::into_raw` | 获取供另一方唯一接管的独立持有，未交出时仍由临时包装负责归还。 |
| `Object::from_transfer` | 由已获真实独立持有的接收者唯一接管。需保证原始表示和所属连接真实有效，且移交只接管一次。 |
| `Object::retain_borrowed` | 在标准初始化的实际借用保护内，从当前输入另取一份独立持有。其安全前提包括当前连接以及来源代码、上下文和诊断存续。 |
| `Object::release` | 结束当前本地拥有权。最后一个拥有者提交释放，完成实际清理由来源及宿主管理推进。 |

```no_run
use pi_ffi_core::object::Object;
use pi_result::Result;

fn retain_and_release(object: &Object) -> Result<u64> {
    let round = object.with_borrowed(|input| {
        // 返回本地数值。闭包退出后不保存原来的地址或借用。
        Ok(input.lease.initialization.round)
    })?;
    let separate = object.try_retain()?;
    // 提交新独立持有的归还，调用者的原对象仍有效。
    separate.release()?;
    Ok(round)
}
```

闭包失败与归还调用保护的失败分别保留。即使借用方法最终报错，已执行的外侧操作也可能有效果。调用方按实际进度处理，而不是另开一个操作重试。

## 使用类型化接口及管理连接（Typed interfaces and connection）

- `AbiInterface` 是不安全特征（unsafe trait），声明固定 `Table`、`ID`、`MAJOR` 及 `SUPPORTED_RIGHTS`。实现者保证表表示、地址存续、源码归属及每次访问的权限合同。
- `Object::interface::<I>` 查询 `Interface<I>`。它共享本地持有；权限每次按真实来源核对。调用者通过 `Interface::with_call` 取得 `InterfaceCall`，在同一次保护内配对使用 `call.table` 和 `call.context`，调用并处理结果。
- `InterfaceCall::lease` 给出本次持有的传输值。`connection` 用于在调用保护内接管合法返回。`decode` 按约定只接管本次真实返回中的独立诊断一次。闭包退回的 Rust 值不能借自本次调用。
- `control::Connection::from_native` 仅用于宿主已确保实际管理视图、配套上下文和存续块的情况。`from_borrowed` 从本次受保护的输入建立连接；`check_peer` 核另一输入的真实表、上下文和轮次。`initialization` 返回本地轮次值。上层接入负责建立外层保护后再走这些不安全入口。

## 发布真实对象及清理来源（Source ownership and cleanup）

`source::Registry::attach` 登记实际来源。用 `ExportedInterface<T>::new::<I>` 关联遵守 `AbiInterface` 的静态 C 表。调用 `new` 时须确保表不可变、尺寸和签名与 I 相符，且来自仍保活的真实来源。随后用 `Registry::export` 发布真实 Rust 值及受控清理。类型要求 `Send + Sync + 'static`。值只在创建来源解释，表宣称类型与实际值的对应由来源负责。

`Registry::with_access` 在该来源取得实际对象访问后，借给同步闭包。`Registry::dispatch` 用于自定义来源 C 回调（C callback）。调用方须确保 `context` 是当前仍保活的该来源，持有与权限遵守相应表合同。调用将本地结果按约定转到 C。`revoke` 阻止新的业务访问，已获准访问依现有保护完成。`maintain` 有界推进实际清理，`try_retire` 检查来源退役。未完成可报告 `Busy` 并保留原进度。具有退出义务的附加状态仍由所有者管理。

```no_run
use pi_ffi_core::{
    object::Object,
    source::{ExportedInterface, Registry},
};
use pi_result::Result;

fn publish<T: Send + Sync + 'static>(
    source: &Registry,
    value: T,
    interfaces: Vec<ExportedInterface<T>>,
    close: impl Fn(&T) -> Result<()> + Send + Sync + 'static,
) -> Result<Object> {
    // 真实适配器准备与 T 对应的只读表，关停闭包记录实际清理结果。
    source.export(value, interfaces, close)
}
```

传给 `export` 的关闭闭包在 `Busy` 时应保有可继续的资源和进度。所有本地独立持有、对象访问和诊断均确认退出后，宿主才结束来源及代码保活。

## 诊断转换和边界保护（Diagnostics and boundary protection）

- `boundary::Context` 保存当前操作、已知模块、轮号与来源。`boundary::catch` 在本地 Rust 构建捕获合同允许的恐慌（panic），保持普通 `pi_result`。调用方须先建立恢复责任。
- `CopiedDiagnostic` 存储宿主自有的原 `user`、`verbose`、`debug` 文本及分类。`into_error` 构成接收方报告，可用 `diagnostic::copied_diagnostic` 读回三段原文本。
- `ManagementFailure::capture` 在来源一侧提取报告文本。原报告在保护内销毁后，用 `finish_report_cleanup` 合并实际结果并保留已有诊断。随后由 `Registry::publish_management_failure` 发布可唯一接管的诊断。来源不可用时，用 `without_source` 返回快照的操作和诊断建立分类。
- `diagnostic::success_outcome`、`limited_outcome`、`status_code` 表达约定的 C 结果及分类。`protect_outcome` 和 `protect_status` 用于各自 C 边界的最外层保护。
- `diagnostic::decode_management` 在模块有效的受保护标准调用中唯一接管管理结果。携带独立诊断时，需要已经匹配来源、实际模块及本轮连接；没有合法来源时只能接收成功或有限失败。

避免以本地报告重新格式化冒充复制来的原文。跨侧复制诊断不会接管来源原生错误、附件、任意分配器对象或代码。捕获恐慌也不会回滚之前的副作用，能否继续取决于已保留的实际状态和清理进度。
