//! 经实际共享适配库（shared adapter library）的公开查询验证同一映射（mapping）内的地址稳定性。
//!
//! 本入口链接实际 pi_ffi_adapters 动态库（dylib），不复制生产实现、不读取地址指向的对象。
//! 覆盖非空、重复和两个线程（thread）各自查询；不证明系统模块归属（module ownership）、
//! 加载器（loader）拒绝边界、卸载（unloading）后行为或 Windows 原生行为。

/// 同一受保护映射（mapping）内重复调用公开查询，地址非空且保持相同。
#[test]
fn test_library_address_is_non_null_and_repeatable() {
    let original = pi_ffi_adapters::library_address();
    assert!(!original.is_null());
    assert_eq!(pi_ffi_adapters::library_address(), original);
}

/// 两个线程（thread）各自经公开入口取址，与主线程（thread）的同一查询结果一致。
#[test]
fn test_library_address_is_stable_across_threads() {
    let original = pi_ffi_adapters::library_address().addr();
    assert_ne!(original, 0);
    let first = std::thread::spawn(|| pi_ffi_adapters::library_address().addr());
    let second = std::thread::spawn(|| pi_ffi_adapters::library_address().addr());
    assert_eq!(first.join().expect("第一个查询线程（thread）正常结束"), original);
    assert_eq!(second.join().expect("第二个查询线程（thread）正常结束"), original);
}
