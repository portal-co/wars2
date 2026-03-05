use std::{
    borrow::Cow, collections::{BTreeMap, BTreeSet}, convert::Infallible, f32::consts::E, iter::once, marker::PhantomData, sync::{Arc, OnceLock}
};
// use pit_core::{Arg, Interface};
use proc_macro2::{Span, TokenStream};
use quasiquote::quasiquote;
use quote::{format_ident, quote, ToTokens};

use sha3::Digest;
use syn::{Ident, Lifetime};

pub(crate) mod pit;
pub struct MemImport {
    pub expr: TokenStream,
    // pub(crate) r#type: TokenStream
}
pub trait Plugin {
    fn pre(&self, module: &mut OptsCore) -> anyhow::Result<()>;
    fn import(
        &self,
        opts: &OptsCore,
        module: &str,
        name: &str,
        params: Vec<TokenStream>,
    ) -> anyhow::Result<Option<TokenStream>>;
    fn mem_import(
        &self,
        opts: &OptsCore,
        module: &str,
        name: &str,
    ) -> anyhow::Result<Option<MemImport>> {
        Ok(None)
    }
    fn post(&self, opts: &OptsCore) -> anyhow::Result<TokenStream>;
    fn bounds(&self, opts: &OptsCore) -> anyhow::Result<Option<TokenStream>> {
        Ok(None)
    }
    fn exref_bounds(&self, opts: &OptsCore) -> anyhow::Result<Option<TokenStream>> {
        Ok(None)
    }
}
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
    pub struct Flags: u32{
        // const HOST_MEMORY = 0x1;
        const ASYNC = 0x2;
        const LEGACY = 0x4;
        // const WASIX = 0x8;
        // const BIND = 0x10;
        // const PIT = 0x20;
        // const UNSANDBOXED = 0x2;
        const NEW_ABI = 0x100;
    }
}
#[cfg(feature = "waffle")]
pub(crate) mod unswitch;
// pub(crate) mod wasix;
pub trait Backend{

}
#[derive(Clone)]
pub struct OptsLt<'a, B,K: Backend> {
    pub module: B,
    pub core: OptsCore<'a>,
    pub backend: PhantomData<K>,
    // pub(crate) cfg: Arc<dyn ImportCfg>,
}
#[derive(Clone)]
pub struct OptsCore<'a> {
    pub crate_path: syn::Path,
    pub bytes: &'a [u8],
    pub name: Ident,
    pub flags: Flags,
    pub embed: TokenStream,
    pub data: BTreeMap<Ident, TokenStream>,
    pub roots: BTreeMap<String, TokenStream>,
    pub plugins: Vec<Arc<dyn Plugin + 'a>>,
}
impl<'a> OptsCore<'a> {
    pub fn inflate<K: Backend>(self) -> OptsLt<'a, &'a [u8], K> {
        OptsLt {
            module: self.bytes,
            core: self,
            backend: PhantomData,
        }
    }
}
pub type Opts<B,K> = OptsLt<'static, B, K>;
#[derive(Clone)]
#[cfg(feature = "waffle")]
pub struct LegacyPortalWaffleBackend;
#[cfg(feature = "waffle")]
impl Backend for LegacyPortalWaffleBackend {}
#[derive(Clone)]
pub struct WasmparserBackend;
impl Backend for WasmparserBackend {}
// pub(crate) trait ImportCfg {
//     fn import(&self, module: &str, name: &str) -> TokenStream;
// }
pub(crate) const INTRINSIC: &'static str = "wars_intrinsic/";
#[cfg(feature = "waffle")]
pub(crate) mod r#impl;
#[cfg(feature = "wasmparser")]
pub(crate) mod new_backend;
pub(crate) mod shared;
