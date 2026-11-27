#![doc(html_no_source)]
#![doc = include_str!("../README.md")]

#[path = "lib_inner.rs"]
mod inner;

pub mod runtime;
pub mod config;
pub mod data;
pub mod service;

/// 返回本共享适配库（shared adapter library）内只读静态对象（static object）的地址（address）。
///
/// 返回值仅用于系统模块归属（module ownership）查询，不得解引用或写入。
/// 对象为本库私有的非零大小静态存储，地址在同一实际映射（mapping）仍受保护时保持相同。
/// 不取得库的所有权（ownership），不得跨卸载（unloading）缓存，也不作为跨进程（process）的稳定身份。
///
/// 可从多个线程（thread）重复调用，不依赖初始化（initialization）轮次（round），不分配、不加锁、不阻塞，
/// 时间与额外空间均为常数。查询不调用外部代码；系统归属核对及错误由调用方处理。
// @trace 1
pub fn library_address() -> *const core::ffi::c_void {
    inner::library_address()
}
