//! Spectest host plugin: implements the standard `spectest` import module
//! (see spec repo `test/core/spectest.wast`) for generated code.
//!
//! Generated modules import from a context trait method per import:
//! `spectest_<name>` (mangled by `bindname`). The harness host context
//! implements those methods, backed by `wars_rt::spectest` support.

use anyhow::Result;
use proc_macro2::TokenStream;
use quote::quote;
use wars::{MemImport, OptsCore, Plugin};

/// Supplies host functions/globals/table/memory for the `spectest` module.
#[derive(Default)]
pub struct SpectestPlugin;

impl SpectestPlugin {
    pub fn boxed() -> std::sync::Arc<dyn Plugin> {
        std::sync::Arc::new(Self)
    }
}

impl Plugin for SpectestPlugin {
    fn pre(&self, _module: &mut OptsCore) -> Result<()> {
        Ok(())
    }

    fn import(
        &self,
        _opts: &OptsCore,
        module: &str,
        name: &str,
        _params: Vec<TokenStream>,
    ) -> Result<Option<TokenStream>> {
        if module != "spectest" {
            return Ok(None);
        }
        // All spectest print functions are result-less sinks. Route them to a
        // context method so the host can observe them if desired. The call
        // expression must evaluate to the empty results tuple-list.
        match name {
            "print" | "print_i32" | "print_i64" | "print_f32" | "print_f64"
            | "print_i32_f32" | "print_f64_f64" => Ok(Some(quote! {{
                ::wars_rt::spectest::print_sink();
                ::wars_rt::_rexport::tuple_list::tuple_list!()
            }})),
            _ => Ok(None),
        }
    }

    fn mem_import(&self, _opts: &OptsCore, module: &str, name: &str) -> Result<Option<MemImport>> {
        if module == "spectest" && name == "memory" {
            return Ok(Some(MemImport {
                expr: quote! { ::wars_rt::spectest::host_memory() },
            }));
        }
        Ok(None)
    }

    fn post(&self, _opts: &OptsCore) -> Result<TokenStream> {
        Ok(quote! {})
    }

    fn bounds(&self, _opts: &OptsCore) -> Result<Option<TokenStream>> {
        Ok(None)
    }

    fn exref_bounds(&self, _opts: &OptsCore) -> Result<Option<TokenStream>> {
        Ok(None)
    }
}
