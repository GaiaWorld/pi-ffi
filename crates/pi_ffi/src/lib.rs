#![doc(html_no_source)]
#![doc = include_str!("../README.md")]

mod authority;
pub mod host;
mod loader;
mod package;
pub mod plugin;

pub use pi_ffi_abi::management as abi;
pub use pi_ffi_adapters::{config, data, runtime, service};
