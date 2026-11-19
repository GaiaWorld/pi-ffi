#![doc(html_no_source)]
#![doc = include_str!("../README.md")]

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
