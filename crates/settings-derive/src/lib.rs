//! Derive macro for generated `settings-core` registries.
//!
//! Русский комментарий: этот crate не владеет настройками приложения. Он только
//! читает metadata рядом со schema struct и генерирует тонкие accessors поверх
//! публичных контрактов `settings-core`.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod codegen;

#[proc_macro_derive(SettingsSchema, attributes(settings, setting))]
pub fn derive_settings_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    codegen::expand_settings_schema(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
