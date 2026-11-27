# 共享适配库地址查询验证（Shared adapter address verification）

`library_address.rs` 直接链接实际 `pi_ffi_adapters` 动态库（dylib），验证公开查询的非空、重复及两个线程（thread）查询一致性。测试不读取地址指向的内容、不复制生产实现，也不新增生产测试接口（API）。它不证明系统模块归属（module ownership）、加载器（loader）拒绝边界、卸载（unloading）后行为或 Windows 原生行为。

在仓库根目录先按同次配套构建（matching build）生成实际适配库（adapter crate）。以下命令复用本批未优化构建位置；换用另一构建时，须一并修改 `--extern`、依赖搜索路径及动态库（dynamic library）搜索路径。

```bash
mkdir -p target/adapter-address
rustc --edition=2021 --test tests/adapter-address/library_address.rs \
    --extern pi_ffi_adapters=target/windows-adapter/build/debug/libpi_ffi_adapters.so \
    -L dependency=target/windows-adapter/build/debug/deps \
    -C prefer-dynamic -o target/adapter-address/library_address_tests
LD_LIBRARY_PATH="target/windows-adapter/build/debug/deps:target/windows-adapter/build/debug:$(rustc --print target-libdir)" \
    target/adapter-address/library_address_tests --test-threads=1 --nocapture
```

仅在 WSL 的 Linux x86_64／GNU 运行本入口。原未实现占位（unimplemented stub）会使这两个测试在公开查询处失败；该结果只证明新增查询尚未实现，不能当作 Windows 导入跳板（import thunk）缺陷已在本地复现。
