#![doc(html_no_source)]
//! 宿主（host）管理与插件（plugin）标准接入。

mod authority;
pub mod host;
mod loader;
mod package;
pub mod plugin;

pub use pi_ffi_abi::management as abi;
pub use pi_ffi_adapters::{config, runtime, service};
