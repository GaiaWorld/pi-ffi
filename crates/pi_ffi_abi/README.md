# pi_ffi_abi

## 用途与支持环境（Scope and support）

`pi_ffi_abi` 定义 Rust 宿主（host）与合规插件动态库（plugin dynamic library）共用的 C 应用二进制接口（C ABI）。它提供固定布局、管理导出、传输身份和函数表（function table）声明，供两侧建立同一协议。库本身为 `no_std`。面向原生插件系统的应用通常从 `pi_ffi` 的 Rust 入口开始。

本项目首批目标平台是 Linux x86_64／GNU 和 Windows x86_64／MSVC。当前已验证的编译器（compiler）是 Rust `1.99.0-nightly`（`eff8269f7`），库采用 Rust 2021 版次（edition）。Linux 构建已核对，Windows 的原生验证（native validation）随整体交付进行。最低支持 Rust 版本（MSRV）尚未确定。

## 使用前的概念（Terminology）

| 名词 | 使用时的含义 |
| --- | --- |
| 协议主版本（protocol major version） | 一组精确声明和调用语义。接口表的身份、主版本和准确字节数共同匹配。 |
| 初始化轮次（initialization round） | 以 `InitIdV1` 识别的一次实际初始化；关闭请求继续用这个标识。 |
| 持有（holding） | `LeaseV1` 指向来源已登记的一次归还责任。复制字段仍指向同一次持有。 |
| 操作标识（operation identifier） | 重放同一实际请求时复用的标识。另一次操作使用新标识。 |
| 借用视图（borrowed view） | 在真实保护内取得的接口表和上下文地址。接收方在每次使用时仍须取得对应资格。 |

## Cargo 功能开关（features）

库清单目前无公开功能开关（features）。直接依赖默认配置即可，因而无额外开关组合或禁止组合。

## 插件公开的三个入口（Management exports）

`management` 模块的 `METADATA_V1`、`INITIALIZE_V1` 和 `CLOSE_V1` 是 `&CStr` 类型的精确 C 符号（C symbol）。对应导出函数类型分别是 `MetadataFnV1`、`InitializeFnV1` 和 `CloseFnV1`。同一产物提供唯一一套导出，宿主在实际模块有效期间核符号归属，按该函数类型调用。

| 声明 | 入参与返回 |
| --- | --- |
| `MetadataV1` | 输出槽接收稳定 UUID、版本字节地址和字节长度。版本是完整 UTF-8 语义化版本（SemVer），宿主在模块保护内复制。 |
| `InitializeFnV1`、`InitReplyV1` | 依次传运行时、终端和配置的只读 `ObjectRefV1` 指针。首项可为空。后两项必传。结果独立给出进度与 `OutcomeV1`。 |
| `INIT_NOT_STARTED`、`INIT_COMPLETED`、`INIT_NEEDS_CLOSE` | 依次表示明确未开始、已经完成、原轮可能有效果需关闭。成功只能与 `INIT_COMPLETED` 配对。 |
| `CloseFnV1` | 传宿主已持有的原轮 `InitIdV1`，接收标准结果。旧标识只能作用于原轮。 |

```rust
use pi_ffi_abi::management::{CLOSE_V1, INITIALIZE_V1, METADATA_V1};

// 这些名字是构建产物中的实际 C 符号。查询时使用完整名称。
assert_eq!(METADATA_V1.to_bytes(), b"pi_ffi_metadata_v1");
assert_eq!(INITIALIZE_V1.to_bytes(), b"pi_ffi_initialize_v1");
assert_eq!(CLOSE_V1.to_bytes(), b"pi_ffi_close_v1");
```

## 身份、借用与公共操作（Identity and access）

| 类型 | 组合使用 |
| --- | --- |
| `UuidV1`、`ApiHeaderV1` | UUID 按 16 字节比较；表头包含主版本和实际表尺寸。 |
| `InitIdV1`、`ObjectKeyV1` | 模块代际及轮号限定初始化。对象完整键由来源、槽位和代际组成。 |
| `LeaseV1`、`OperationIdV1` | 持有还包含对象键和轮号。逻辑操作含发起者与序号，同操作重试必须保持全部入参。 |
| `ObjectRefV1` | 将来源上下文、`SourceApiV1` 和其有效持有一起传输。Rust 的裸地址或非空检查本身无法建立实际保护。 |
| `InterfaceRequestV1`、`InterfaceViewV1` | 请求携带准确的接口身份、主版本和尺寸。借用结果是所请求的只读接口表及其配对上下文。 |
| `SourceApiV1` | `validate` 作即时校验。`retain` 获得独立持有。`release` 归还特定持有。`query_interface` 取得指定接口的借用视图。每次真正调用仍核资格。 |

```rust
use core::mem::size_of;
use pi_ffi_abi::{HostControlApiV1, InterfaceRequestV1, HOST_CONTROL_ID, HOST_CONTROL_MAJOR};

// 只构造请求描述。来源仍会核同轮持有及表的真实归属。
let request = InterfaceRequestV1 {
    interface_id: HOST_CONTROL_ID,
    major: HOST_CONTROL_MAJOR,
    size: size_of::<HostControlApiV1>() as u32,
};
assert_eq!(request.size as usize, size_of::<HostControlApiV1>());
```

`query_interface` 只在成功时让输出槽有效。调用方在作用域内保活本次代码、来源上下文和持有，依表声明核对函数存在、权限和槽位，再读取成功输出。使用同一返回视图内配对的表和上下文。普通对象接口对应其实际来源；管理接口的表和受限上下文来自宿主管理权威。查询时使用的租约仍属于原对象来源。

## 操作结果、诊断及管理表（Results, diagnostics, and control）

- `OutcomeV1` 成功时两个分类字段为零且诊断为空。一般失败持有独立的 `DiagnosticRefV1`。建立诊断失败时，两个分类字段分别记录原操作和诊断失败，诊断为空。
- `DiagnosticInfoV1` 包含分类与 `user`、`verbose`、`debug` 三段的 UTF-8 字节长度。由 `DiagnosticApiV1.info` 读取声明，再用 `copy` 复制到宿主缓冲区，最后用 `release` 归还实际来源。三段长度与写入量须核对。已接管且匹配的独立诊断即使读取或复制失败也须在原保护内归还一次。只读取及归还已核实来源与持有的诊断。
- `CallTargetV1` 绑定真实上下文、表、完整持有与目标种类 `TARGET_OBJECT` 或 `TARGET_DIAGNOSTIC`。`CallTokenV1` 用于归还已经获得的某次调用资格。两种标识的普通字段复制没有额外副作用。
- `SourceDescriptorV1` 提供来源上下文、对象表、诊断表和控制表。登记时的宿主管理上下文和操作标识决定初始化作用域。`SourceControlApiV1` 的 `maintain`、`try_retire`、`release_context` 分别推进真实收尾、核退役和释放空的来源上下文。`MaintenanceV1` 描述本次清理的瞬时结果。
- `HostControlApiV1` 的 `register_source`、`next_operation`、`track_holding`、`request_release`、`enter_call`、`leave_call`、`retire_source` 共同承接权限与释放。管理表用 `HOST_CONTROL_ID`、`HOST_CONTROL_MAJOR` 请求，来源控制表按 `SOURCE_CONTROL_MAJOR` 核验。普通包装者通过上层 Rust 接口操作。

在适用的 C 边界内将可展开恐慌（unwinding panic）转为约定结果，各侧维持自己的实际生命周期（lifecycle）。尽量通过 `pi_ffi_core` 和 `pi_ffi` 处理受控地址、持有、版本及 `pi_result` 分类。
