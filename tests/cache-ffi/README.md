# 共享缓存真实进程测试（cache-ffi）

本目录是常驻的真实进程集成测试（real-process integration test）。`host.rs` 加载 `plugin.rs` 产生的动态库（dynamic library），`client.rs` 使用原 `pi_shared_cache_client` 访问原共享缓存服务。完整集成入口 `run.py` 在 WSL 的 Linux x86_64／GNU 环境串行运行。Windows 使用单轮基本验证入口。

首次准备固定依赖，然后运行：

```bash
python3 tests/cache-ffi/prepare.py
python3 tests/cache-ffi/run.py
```

基础依赖包默认位于 `docs/evidence/EV-0018/cache-ffi-dependencies.tar.gz`，`pi_p2p` 修复源码包默认位于 `docs/evidence/EV-0023/pi_p2p-1.3.22.tar.gz`。两包的摘要都由 `dependencies.json` 固定。换一份检出（checkout）时，可分别通过 `--archive` 和 `--p2p-archive` 指定归档文件的实际位置。已准备的 `1.3.18` 缓存只需安装一次修复源码，保留其它固定输入。后续运行会核对 `pi_p2p` 源码摘要并复用缓存。需要本仓配套的 `../pi_result`、`../pi-async-rt_v1.0`、`../pi_task_pool`、`../pi-clock`、Rust nightly、Cargo、Python 3 和 `nm`。构建前后记录相邻库的源码、过程宏（procedural macro）及构建脚本摘要。

本测试使用缓存服务（cache service）和客户端（client）`0.2.3`、`pi_p2p 1.3.22` 提交（commit）`dd90c86f84b6fc537f2d8e571ca60a05a8c2b910`、`pi_p2p_service 0.3.11`、`pi_service 0.6.3` 及 Chitchat `0.10.1`。Cargo 离线按锁文件（lockfile）构建。`pi_p2p` 的提交源码来自本地归档。原 59 个静态服务（static service）端口为 3941—3999，业务断言（assertion）覆盖其中 13 类接口。

源码、依赖清单、工具链（toolchain）和制品摘要匹配时，`run.py` 直接复用 `target/cache-ffi/artifacts/<形式>` 中的制品，不执行 Cargo。需要重新构建时使用 `target/cache-ffi/build` 的增量缓存；测试源码和编译制品都在仓库。公共依赖以一个压缩包常驻缓存，只有需要编译时才在 WSL 临时解包，避免把大量小文件反复写入 Windows 磁盘；解包目录用完自动清理。可以明确要求只复用已有匹配制品：

```bash
python3 tests/cache-ffi/run.py --no-build
```

默认严格串行验证手工包装、过程宏（procedural macro）和可选运行时类型的三种形式，每种分别使用初始化顺序 `121`、`212`。每种形式的两种顺序必须使用同一份双版动态库。每组两次成功初始化、一次真实失败初始化、105 次原生进程间通信（IPC）且没有业务回退、18 次实际元信息（metadata）查询；另有四项真实数据输入失败边界。可选运行时形式使用实际 `Handle` 类型，但传入值为 `None`，不证明 `Some` 情形。

用同一可选形式验证实际传入 `Some`。三个初始化轮次各使用不同的宿主运行时（runtime）。插件提交的任务（task）由相应所属线程（owner thread）实际推进。关闭后验证输入归还以及宿主继续完成任务，最后由宿主主动关闭所有实例。

```bash
python3 tests/cache-ffi/run.py --form optional --runtime some
```

默认入口验证功能。WSL 可显式指定 `--repeat` 执行 1 至 8 次串行重复，每次使用独立的宿主与客户端（client）进程及运行目录。`--release` 选用独立的优化制品（optimized artifact），可与 `--no-build` 一起复用。成功轮次的宿主日志包含 `ffi_timing`，分别记录装入、初始化、关闭及管理卸载的纳秒耗时。客户端日志的 `service_rpc_finish` 含原生进程间通信（IPC）请求的毫秒耗时。Python 需启用断言（assertion）。

定位某一组合时可缩小运行范围；这不等同完整默认组通过：

```bash
python3 tests/cache-ffi/run.py --form manual --sequence 121
```

标准初始化（initialization）的结果取自接口实际返回值。成功返回后，宿主（host）立即核对本机注册表（registry）中 59 个固定端口都为运行中（Running）。客户端（client）仍按原 20 秒截止时间等到远端发现（discovery）记录中 59 个端口都为运行中，再执行业务。业务完成后、关闭前，宿主再核对端口 3959 的本机记录版本和内容。插件内部服务管理透明。关闭后另有宿主控制调用验证共享输入仍可使用。发现和建链使用原生 QUIC，同机业务逐次核对实际进程间通信（IPC）路径。

完整运行记录保存在 `target/cache-ffi/results/<时间>`。每次仅把进程协调、测试数据和独立部署放进本次自建 `/tmp/pi-ffi-cache-run-*`。成功后先复制记录并逐文件核对摘要，再清理该临时目录。失败时保留现场并打印路径，归档核验后再清理。常驻源码及构建缓存保留供下次使用。

若服务发现等待达到 10 秒，客户端保存一次只读观察；若宿主等待业务完成失败，则在继续抛出原错误前保存观察。记录包含原注册表、公开发现结果及对应本地状态，各层分别读取，不承诺同一时刻的一致快照，不修改或补写发现记录，也不延长截止时间。

## Windows 原生基本验证

在 Windows 的 x64 Visual Studio 开发者 PowerShell 中，进入本仓根目录。启用 x64/MSVC Rust nightly，使用 Python 3 启动。固定配套（dependencies）使用本目录已有的 `target/cache-ffi` 准备结果，Windows 制品（artifacts）和结果单独保存于 `target/cache-ffi-windows`。

```powershell
py -3 -B .\tests\cache-ffi\windows\basic.py
```

本入口只跑一轮。宿主（host）用标准入口装载 `.dll`、读取实际元信息（metadata）并执行 v2 初始化。独立客户端（client）创建临时表、写入并读回预期值。标准关闭之后，客户端调用宿主控制服务。四次请求都须是原生进程间通信（IPC），回退（fallback）为 `0`。两进程均须退出 `0`。

最后一行打印 `PASS` 或 `FAIL` 及结果压缩包（ZIP archive）的完整路径。即使在构建或装入阶段失败，也可回传该压缩包。它包含 `result.json` 和当时产生的构建、宿主及客户端日志。首次构建时间取决于本机已有缓存。
