use alloc::vec;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::marker::PhantomData;
use core::mem::transmute;
use core::{
    future::Future,
    iter::{empty, once},
    pin::Pin,
};
use anyhow::Context;
pub use super::value;
// use tramp::{tramp, BorrowRec, Thunk};
pub fn ret<'a, T>(a: T) -> AsyncRec<'a, T> {
    AsyncRec::Ret(a)
}
pub enum AsyncRec<'a, T> {
    Ret(T),
    Async(Pin<Box<dyn UnwrappedAsyncRec<'a, T>>>),
}
pub trait UnwrappedAsyncRec<'a, T>: Future<Output = AsyncRec<'a, T>> + Send + Sync + 'a {
    async fn go(mut self) -> T
    where
        Self: Sized,
    {
        return self.await.go().await;
    }
}
pub trait Wrap<'a,T>: Sized{
    fn wrap(self) -> AsyncRec<'a,T>;
}
impl<'a,T> Wrap<'a,T> for AsyncRec<'a,T>{
    fn wrap(self) -> AsyncRec<'a,T> {
        self
    }
}
impl<'a,T,F: UnwrappedAsyncRec<'a,T>> Wrap<'a,T> for F{
    fn wrap(self) -> AsyncRec<'a,T> {
        AsyncRec::Async(Box::pin(self))
    }
}
impl<'a, T, F: Future<Output = AsyncRec<'a, T>> + Send + Sync + 'a> UnwrappedAsyncRec<'a, T> for F {}
impl<'a, T> AsyncRec<'a, T> {
    pub fn wrap(x: impl Wrap<'a,T>) -> Self{
        x.wrap()
    }
    pub async fn go(mut self) -> T {
        loop {
            self = match self {
                AsyncRec::Ret(r) => return r,
                AsyncRec::Async(a) => a.await,
            }
        }
    }
}
pub use crate::CtxSpec;
use crate::Traverse;
use crate::func::value::ForLt;
#[repr(transparent)]
pub struct Value<C: CtxSpec>(pub super::value::Value<C, AsyncForLt<C>>);
pub struct AsyncForLt<C: CtxSpec> {
    ph: PhantomData<C>,
}
impl<'a, C: CtxSpec> ForLt<'a> for AsyncForLt<C> {
    type ForLt = AsyncRec<'a, anyhow::Result<Vec<Value<C>>>>;
}
pub fn call_ref<'a, A: CoeVec<C> + 'static, B: CoeVec<C> + 'static, C: CtxSpec + 'static>(
    ctx: &'a mut C,
    go: Df<A, B, C>,
    a: A,
) -> AsyncRec<'a, anyhow::Result<B>> {
    // let go: Df<A, B, C> = cast(go);
    go(ctx, a)
}
#[cfg(feature = "dumpster")]
const _: () = {
    use dumpster::Trace;
    unsafe impl<C: CtxSpec<ExternRef: Trace>> Trace for Value<C> {
        fn accept<V: dumpster::Visitor>(&self, visitor: &mut V) -> Result<(), ()> {
            self.0.accept(visitor)
        }
    }
};
impl<C: CtxSpec> Traverse<C> for Value<C> {
    fn traverse<'a>(&'a self) -> Box<dyn Iterator<Item = &'a <C as CtxSpec>::ExternRef> + 'a> {
        self.0.traverse()
    }
    fn traverse_mut<'a>(
        &'a mut self,
    ) -> Box<dyn Iterator<Item = &'a mut <C as CtxSpec>::ExternRef> + 'a> {
        self.0.traverse_mut()
    }
}
impl<C: CtxSpec> Clone for Value<C> {
    fn clone(&self) -> Self {
       Self(self.0.clone())
    }
}
pub trait Coe<C: CtxSpec>: Sized {
    fn coe(self) -> Value<C>;
    fn uncoe(x: Value<C>) -> anyhow::Result<Self>;
}
pub fn cast<A: Coe<C> + 'static, B: Coe<C> + 'static, C: CtxSpec>(a: A) -> B {
    let a = match castaway::cast!(a, B) {
        Ok(b) => return b,
        Err(a) => a,
    };
    B::uncoe(A::coe(a)).unwrap()
}
impl<C: CtxSpec> Coe<C> for Value<C> {
    fn coe(self) -> Value<C> {
        self
    }
    fn uncoe(x: Value<C>) -> anyhow::Result<Self> {
        Ok(x)
    }
}
impl<C: CtxSpec, D: Coe<C>> Coe<C> for Option<D> {
    fn coe(self) -> Value<C> {
        match self {
            None => Value(super::value::Value::Null),
            Some(d) => d.coe(),
        }
    }
    fn uncoe(x: Value<C>) -> anyhow::Result<Self> {
        if let super::value::Value::Null = &x.0 {
            return Ok(None);
        }
        return Ok(Some(D::uncoe(x)?));
    }
}
macro_rules! coe_impl_prim {
    ($a:tt in $b:ident) => {
        impl<C: CtxSpec> Coe<C> for $a {
            fn coe(self) -> Value<C> {
                Value(super::value::Value::$b(self))
            }
            fn uncoe(x: Value<C>) -> anyhow::Result<Self> {
                match x.0 {
                    super::value::Value::$b(a) => Ok(a),
                    _ => anyhow::bail!("invalid type"),
                }
            }
        }
    };
}
coe_impl_prim!(u32 in I32);
coe_impl_prim!(u64 in I64);
coe_impl_prim!(f32 in F32);
coe_impl_prim!(f64 in F64);
#[cfg(feature = "dumpster")]
pub trait CoeField<C: CtxSpec>: Sized {
    fn coe(self) -> crate::gc::Field<Value<C>>;
    fn uncoe(x: crate::gc::Field<Value<C>>) -> anyhow::Result<Self>;
}
#[cfg(feature = "dumpster")]
pub trait CoeFieldVec<C: CtxSpec>: Sized {
    const NUM: usize;
    fn coe(self) -> Vec<crate::gc::Field<Value<C>>>;
    fn uncoe(a: Vec<crate::gc::Field<Value<C>>>) -> anyhow::Result<Self>;
}
#[cfg(feature = "dumpster")]
const _: () = {
    use std::sync::Mutex;
    use crate::gc::{Const, Field, Mut, Struct};
    impl<C: CtxSpec, V: Coe<C>> CoeField<C> for Const<V> {
        fn coe(self) -> crate::gc::Field<Value<C>> {
            crate::gc::Field::Const(self.0.coe())
        }
        fn uncoe(x: crate::gc::Field<Value<C>>) -> anyhow::Result<Self> {
            V::uncoe(match x {
                crate::gc::Field::Const(a) => a,
                crate::gc::Field::Mut(arc) => arc.lock().unwrap().clone(),
            })
            .map(Self)
        }
    }
    impl<C: CtxSpec, V: Coe<C>> CoeField<C> for Mut<V> {
        fn coe(self) -> crate::gc::Field<Value<C>> {
            crate::gc::Field::Mut(Arc::new(Mutex::new(self.0.coe())))
        }
        fn uncoe(x: crate::gc::Field<Value<C>>) -> anyhow::Result<Self> {
            V::uncoe(match x {
                crate::gc::Field::Const(a) => a,
                crate::gc::Field::Mut(arc) => arc.lock().unwrap().clone(),
            })
            .map(Self)
        }
    }
    impl<C: CtxSpec> CoeFieldVec<C> for () {
        fn coe(self) -> Vec<Field<Value<C>>> {
            vec![]
        }
        fn uncoe(a: Vec<Field<Value<C>>>) -> anyhow::Result<Self> {
            Ok(())
        }
        const NUM: usize = 0;
    }
    impl<C: CtxSpec, A: CoeField<C>, B: CoeFieldVec<C>> CoeFieldVec<C> for (A, B) {
        fn coe(self) -> Vec<Field<Value<C>>> {
            let mut a = self.1.coe();
            a.push(self.0.coe());
            return a;
        }
        fn uncoe(mut a: Vec<Field<Value<C>>>) -> anyhow::Result<Self> {
            let Some(x) = a.pop() else {
                anyhow::bail!("list too small")
            };
            let y = A::uncoe(x).context("invalid item (note coe lists are REVERSED)")?;
            let z = B::uncoe(a)?;
            Ok((y, z))
        }
        const NUM: usize = B::NUM + 1;
    }
     impl<C: CtxSpec, V: CoeFieldVec<C>> Coe<C> for Struct<V> {
        fn coe(self) -> Value<C> {
            Value(super::value::Value::Gc(crate::gc::GcCore::Fields(
                match self.0.coe() {
                    a => unsafe {
                        use core::mem::transmute;
                        transmute(a)
                    },
                },
            )))
        }
        fn uncoe(x: Value<C>) -> anyhow::Result<Self> {
            match x.0 {
                super::value::Value::Gc(crate::gc::GcCore::Fields(f)) => V::uncoe(unsafe {
                    use core::mem::transmute;
                    transmute(f)
                })
                .map(Self),
                _ => anyhow::bail!("nota gc"),
            }
        }
    }
};
pub trait CoeVec<C: CtxSpec>: Sized {
    const NUM: usize;
    fn coe(self) -> Vec<Value<C>>;
    fn uncoe(a: Vec<Value<C>>) -> anyhow::Result<Self>;
}
impl<C: CtxSpec> CoeVec<C> for () {
    fn coe(self) -> Vec<Value<C>> {
        vec![]
    }
    fn uncoe(a: Vec<Value<C>>) -> anyhow::Result<Self> {
        Ok(())
    }
    const NUM: usize = 0;
}
impl<C: CtxSpec, A: Coe<C>, B: CoeVec<C>> CoeVec<C> for (A, B) {
    fn coe(self) -> Vec<Value<C>> {
        let mut a = self.1.coe();
        a.push(self.0.coe());
        return a;
    }
    fn uncoe(mut a: Vec<Value<C>>) -> anyhow::Result<Self> {
        let Some(x) = a.pop() else {
            anyhow::bail!("list too small")
        };
        let y = A::uncoe(x).context("invalid item (note coe lists are REVERSED)")?;
        let z = B::uncoe(a)?;
        Ok((y, z))
    }
    const NUM: usize = B::NUM + 1;
}
pub fn map_rec<'a, T: 'a, U>(
    r: AsyncRec<'a, T>,
    go: impl FnOnce(T) -> U + Send + Sync + 'a,
) -> AsyncRec<'a, U> {
    match r {
        AsyncRec::Ret(x) => AsyncRec::Ret(go(x)),
        AsyncRec::Async(a) => AsyncRec::Async(Box::pin(async move {
            let v = a.await;
            map_rec(v, go)
        })),
    }
}
pub type Df<A, B, C> =
    Arc<dyn for<'a> Fn(&'a mut C, A) -> AsyncRec<'a, anyhow::Result<B>> + Send + Sync + 'static>;
pub fn da<
    A,
    B,
    C,
    F: for<'a> Fn(&'a mut C, A) -> AsyncRec<'a, anyhow::Result<B>> + Send + Sync + 'static,
>(
    f: F,
) -> Df<A, B, C> {
    Arc::new(f)
}
impl<C: CtxSpec + 'static, A: CoeVec<C> + 'static, B: CoeVec<C> + 'static> Coe<C> for Df<A, B, C> {
    fn coe(self) -> Value<C> {
        pub fn x<
            C: CtxSpec,
            T: for<'a> Fn(&'a mut C, Vec<super::value::Value<C,AsyncForLt<C>>>) -> AsyncRec<'a, anyhow::Result<Vec<Value<C>>>>
                + 'static,
        >(
            a: T,
        ) -> T {
            return a;
        }
        Value(super::value::Value::FunRef(Arc::new(x(move |ctx, x| {
            let x = match A::uncoe(unsafe{transmute(x)}) {
                Ok(x) => x,
                Err(e) => return AsyncRec::Ret(Err(e)),
            };
            let x = self(ctx, x);
            map_rec(x, |a| a.map(|b| b.coe()))
        }))))
    }
    fn uncoe(x: Value<C>) -> anyhow::Result<Self> {
        let super::value::Value::FunRef(x) = x.0 else {
            anyhow::bail!("invalid value")
        };
        Ok(Arc::new(move |ctx, a| {
            let v = a.coe();
            let v = x(ctx, unsafe{
                transmute(v)
            });
            map_rec(v, |a| a.and_then(B::uncoe))
        }))
    }
}
