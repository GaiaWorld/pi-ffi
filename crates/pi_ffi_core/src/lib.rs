#![doc(html_no_source)]
//! 受控对象交接（controlled object handoff）、诊断与来源生命周期。
//! 标准C入口需要在其自身边界捕获可展开恐慌，不跨C展开。

pub mod boundary;
pub mod control;
pub mod diagnostic;
pub mod object;
pub mod source;

// 共享封装的编译期约束（compile-time constraint）；不产生运行期状态。
struct Concurrent<T: Send + Sync>(std::marker::PhantomData<T>);
const _: Option<Concurrent<control::Connection>> = None;
const _: Option<Concurrent<object::Object>> = None;
const _: Option<Concurrent<object::TransferObject>> = None;
const _: Option<Concurrent<source::Registry>> = None;
