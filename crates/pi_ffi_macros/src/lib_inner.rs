//! 编译期解析及标准入口的生成实现。

use super::PluginDeclaration;
use proc_macro::TokenStream;
use syn::parse::ParseStream;

// @trace 1
pub(super) fn export_plugin(input: TokenStream) -> TokenStream {
    todo!()
}

// @trace 2
pub(super) fn parse_declaration(input: ParseStream<'_>) -> syn::Result<PluginDeclaration> {
    todo!()
}
