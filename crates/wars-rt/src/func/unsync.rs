use super::*;
use core::future::Future;
use core::pin::Pin;

pub trait AsyncRec<'a, T> {
    fn go(self) -> Pin<Box<dyn Future<Output = T> + 'a>>;
    fn wrap(res: BorrowRec<'a, T>) -> Self;
}

impl<'a, T: 'a> AsyncRec<'a, T> for BorrowRec<'a, T> {
    fn go(self) -> Pin<Box<dyn Future<Output = T> + 'a>> {
        Box::pin(async move {
            tramp(self)
        })
    }
    fn wrap(res: BorrowRec<'a, T>) -> Self {
        res
    }
}

pub trait UnwrappedAsyncRec<'a, T>: Future<Output = T> + 'a {
}

impl<'a, T: 'a, F: Future<Output = T> + 'a> UnwrappedAsyncRec<'a, T> for F {
}

pub struct Df<A, B, C: CtxSpec> {
    pub inner: Arc<dyn for<'a> Fn(&'a mut C, A) -> Pin<Box<dyn Future<Output = Result<B, C::Error>> + 'a>> + Send + Sync + 'static>,
}

impl<A, B, C: CtxSpec> Clone for Df<A, B, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<C: CtxSpec + 'static, A: CoeVec<C> + 'static, B: CoeVec<C> + 'static> Coe<C> for Df<A, B, C> {
    fn coe(self) -> Value<C> {
        let inner = self.inner;
        Value(value::Value::FunRef(Arc::new(move |ctx, args| {
            let args = match A::uncoe(unsafe { core::mem::transmute(args) }) {
                Ok(a) => a,
                Err(e) => return BorrowRec::Ret(Err(e)),
            };
            let fut = (inner)(ctx, args);
            // We use transmute_copy to bridge the gap between the expected return type
            // and the Pin<Box<dyn Future>>. This async backend relies on unsound transmutes
            // that assume all possible return types of this closure have the same size
            // as Pin<Box<dyn Future>> (64 bits).
            BorrowRec::Ret(unsafe { 
                let f = Box::pin(async move {
                    let res: Result<B, C::Error> = fut.await;
                    res.map(|b| b.coe())
                });
                core::mem::transmute_copy(&f)
            })
        })))
    }

    fn uncoe(x: Value<C>) -> Result<Self, C::Error> {
        let value::Value::FunRef(inner) = x.0 else {
            // return Err(anyhow::Error::msg("not a funcref").into());
            todo!()
        };
        Ok(Df {
            inner: Arc::new(move |ctx, args| {
                let v: Vec<Value<C>> = args.coe();
                let res = inner(ctx, unsafe { core::mem::transmute(v) });
                Box::pin(async move {
                    let res: Result<Vec<Value<C>>, C::Error> = tramp(res);
                    res.and_then(B::uncoe)
                })
            }),
        })
    }
}
