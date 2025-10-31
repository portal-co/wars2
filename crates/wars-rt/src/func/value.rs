use super::*;
pub trait ForLt<'a>{
    type ForLt;
}
#[non_exhaustive]
pub enum Value<C: CtxSpec,R: for<'a>ForLt<'a>> {
    I32(u32),
    I64(u64),
    F32(f32),
    F64(f64),
    FunRef(
        Arc<
            dyn for<'a> Fn(
                    &'a mut C,
                    Vec<Value<C,R > >,
                ) -> <R as ForLt<'a>>::ForLt
                + Send
                + Sync
                + 'static,
        >,
    ),
    Null,
    ExRef(C::ExternRef),
    #[cfg(feature = "dumpster")]
    Gc(crate::gc::GcCore<Value<C,R>>),
}
#[cfg(feature = "dumpster")]
const _: () = {
    use dumpster::Trace;
    unsafe impl<C: CtxSpec<ExternRef: Trace>,R:for<'a>ForLt<'a>> Trace for Value<C,R> {
        fn accept<V: dumpster::Visitor>(&self, visitor: &mut V) -> Result<(), ()> {
            match self {
                Self::ExRef(e) => e.accept(visitor),
                Self::Gc(g) => g.accept(visitor),
                _ => Ok(()),
            }
        }
    }
};
impl<C: CtxSpec,R:for<'a>ForLt<'a>> Traverse<C> for Value<C,R> {
    fn traverse<'a>(&'a self) -> Box<dyn Iterator<Item = &'a <C as CtxSpec>::ExternRef> + 'a> {
        match self {
            Value::ExRef(e) => Box::new(once(e)),
            #[cfg(feature = "dumpster")]
            Value::Gc(g) => g.traverse(),
            _ => Box::new(empty()),
        }
    }
    fn traverse_mut<'a>(
        &'a mut self,
    ) -> Box<dyn Iterator<Item = &'a mut <C as CtxSpec>::ExternRef> + 'a> {
        match self {
            Value::ExRef(e) => Box::new(once(e)),
            #[cfg(feature = "dumpster")]
            Value::Gc(g) => g.traverse_mut(),
            _ => Box::new(empty()),
        }
    }
}
impl<C: CtxSpec,R: for<'a>ForLt<'a>> Clone for Value<C,R> {
    fn clone(&self) -> Self {
        match self {
            Self::I32(arg0) => Self::I32(arg0.clone()),
            Self::I64(arg0) => Self::I64(arg0.clone()),
            Self::F32(arg0) => Self::F32(arg0.clone()),
            Self::F64(arg0) => Self::F64(arg0.clone()),
            Self::FunRef(arg0) => Self::FunRef(arg0.clone()),
            Self::Null => Self::Null,
            Self::ExRef(e) => Self::ExRef(e.clone()),
            #[cfg(feature = "dumpster")]
            Self::Gc(c) => Self::Gc(c.clone()),
        }
    }
}