#![doc(html_no_source)]
#![doc = include_str!("../README.md")]

#[cfg(all(any(feature = "init_v1", feature = "init_v2"), feature = "metadata_v1", feature = "close_v1"))]
mod authority;
#[cfg(all(any(feature = "init_v1", feature = "init_v2"), feature = "metadata_v1", feature = "close_v1"))]
pub mod host;
#[cfg(all(any(feature = "init_v1", feature = "init_v2"), feature = "metadata_v1", feature = "close_v1"))]
mod loader;
mod package;
#[cfg(all(any(feature = "init_v1", feature = "init_v2"), feature = "metadata_v1", feature = "close_v1"))]
pub mod plugin;

pub use pi_ffi_abi::management as abi;
pub use pi_ffi_adapters::{config, data, runtime, service};

#[cfg(not(any(feature = "init_v1", feature = "init_v2")))]
compile_error!("必须启用至少一个初始化版本（initialization version）：init_v1 或 init_v2");
#[cfg(not(feature = "metadata_v1"))]
compile_error!("必须启用元信息版本（metadata version）：metadata_v1");
#[cfg(not(feature = "close_v1"))]
compile_error!("必须启用关闭版本（close version）：close_v1");
