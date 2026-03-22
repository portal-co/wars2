#![no_std]
extern crate alloc;
pub use core::convert::Infallible;
pub use either::Either;
pub mod func;
#[cfg(feature = "dumpster")]
pub mod gc;
pub mod wasix;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{error::Error, iter::empty};
#[cfg(not(feature = "std"))]
pub use spin::Mutex;
#[cfg(feature = "std")]
pub use std::sync::Mutex;

pub trait Err: Into<anyhow::Error> {}
impl<T: Into<anyhow::Error>> Err for T {}

#[cfg(feature = "std")]
extern crate std;
#[derive(Clone)]
pub enum Pit<X, H> {
    Guest { id: [u8; 32], x: X, s: [u8; 32] },
    Host { host: H },
}
// use as_ref::AsSlice;
// use func::CtxSpec;
pub use func::Value;

pub trait CtxSpec: Sized {
    type ExternRef: Clone;
    type Error: Error + 'static;
}

pub trait Traverse<C: CtxSpec> {
    fn traverse<'a>(&'a self) -> Box<dyn Iterator<Item = &'a C::ExternRef> + 'a>;
    fn traverse_mut<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a mut C::ExternRef> + 'a>;
}
impl<C: CtxSpec, V: Traverse<C>> Traverse<C> for Vec<V> {
    fn traverse<'a>(&'a self) -> Box<dyn Iterator<Item = &'a <C as CtxSpec>::ExternRef> + 'a> {
        Box::new(self.iter().flat_map(|a| a.traverse()))
    }
    fn traverse_mut<'a>(
        &'a mut self,
    ) -> Box<dyn Iterator<Item = &'a mut <C as CtxSpec>::ExternRef> + 'a> {
        Box::new(self.iter_mut().flat_map(|x| x.traverse_mut()))
    }
}
impl<C: CtxSpec> Traverse<C> for u32 {
    fn traverse<'a>(&'a self) -> Box<dyn Iterator<Item = &'a <C as CtxSpec>::ExternRef> + 'a> {
        Box::new(empty())
    }
    fn traverse_mut<'a>(
        &'a mut self,
    ) -> Box<dyn Iterator<Item = &'a mut <C as CtxSpec>::ExternRef> + 'a> {
        Box::new(empty())
    }
}
impl<C: CtxSpec> Traverse<C> for u64 {
    fn traverse<'a>(&'a self) -> Box<dyn Iterator<Item = &'a <C as CtxSpec>::ExternRef> + 'a> {
        Box::new(empty())
    }
    fn traverse_mut<'a>(
        &'a mut self,
    ) -> Box<dyn Iterator<Item = &'a mut <C as CtxSpec>::ExternRef> + 'a> {
        Box::new(empty())
    }
}
pub trait Memory<E> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        let r = self.read(a, s)?;
        handle(r.as_ref().as_ref())
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E>;
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E>;
    fn size(&self) -> Result<u64, E>;
    fn grow(&mut self, x: u64) -> Result<(), E>;
}
#[cfg(feature = "ic-stable-structures")]
pub mod ic {
    use alloc::{boxed::Box, vec};
    #[repr(transparent)]
    pub struct Stable<T>(pub T);
    impl<T: ic_stable_structures::Memory> super::Memory<anyhow::Error> for Stable<T> {
        fn read<'a>(&'a self, a: u64, s: u64) -> anyhow::Result<Box<dyn AsRef<[u8]> + 'a>> {
            let mut v = vec![0u8; s as usize];
            self.0.read(a, &mut v);
            Ok(Box::new(v))
        }
        fn write(&mut self, a: u64, x: &[u8]) -> anyhow::Result<()> {
            self.0.write(a, x);
            Ok(())
        }
        fn size(&self) -> anyhow::Result<u64> {
            let s = self.0.size();
            Ok(s * 65536)
        }
        fn grow(&mut self, x: u64) -> anyhow::Result<()> {
            if self.0.grow((x + 65535) / 65536) == -1 {
                anyhow::bail!("stable growth failed")
            }
            Ok(())
        }
    }
}
impl<E> Memory<E> for Vec<u8> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        let r = &self[(a as usize)..][..(s as usize)];
        handle(r)
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        Ok(Box::new(&self[(a as usize)..][..(s as usize)]))
    }
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E> {
        self[(a as usize)..][..x.len()].copy_from_slice(x);
        Ok(())
    }
    fn size(&self) -> Result<u64, E> {
        Ok(self.len() as u64)
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        self.extend((0..x).map(|_a| 0u8));
        Ok(())
    }
}
impl<T: Memory<E> + ?Sized, E> Memory<E> for Box<T> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        self.as_ref().read2(a, s, handle)
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        self.as_ref().read(a, s)
    }
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E> {
        self.as_mut().write(a, x)
    }
    fn size(&self) -> Result<u64, E> {
        self.as_ref().size()
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        self.as_mut().grow(x)
    }
}
#[cfg(feature = "std")]
impl<T: Memory<E>, E> Memory<E> for Arc<std::sync::Mutex<T>> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        let l = self.lock().unwrap();
        return l.read2(a, s, handle);
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        let l = self.lock().unwrap();
        let r = l.read(a, s)?;
        return Ok(Box::new(r.as_ref().as_ref().to_vec()));
    }
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E> {
        let mut l = self.lock().unwrap();
        return l.write(a, x);
    }
    fn size(&self) -> Result<u64, E> {
        let l = self.lock().unwrap();
        return l.size();
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        let mut l = self.lock().unwrap();
        return l.grow(x);
    }
}
#[cfg(feature = "std")]
impl<T: Memory<E>, E> Memory<E> for Arc<std::sync::RwLock<T>> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        let l = std::sync::RwLock::read(&self).unwrap();
        return l.read2(a, s, handle);
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        let l = std::sync::RwLock::read(&self).unwrap();
        let r = l.read(a, s)?;
        return Ok(Box::new(r.as_ref().as_ref().to_vec()));
    }
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E> {
        let mut l = std::sync::RwLock::write(&self).unwrap();
        return l.write(a, x);
    }
    fn size(&self) -> Result<u64, E> {
        let l = std::sync::RwLock::read(&self).unwrap();
        return l.size();
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        let mut l = std::sync::RwLock::write(&self).unwrap();
        return l.grow(x);
    }
}
impl<T: Memory<E>, E> Memory<E> for Arc<spin::Mutex<T>> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        let l = self.lock();
        return l.read2(a, s, handle);
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        let l = self.lock();
        let r = l.read(a, s)?;
        return Ok(Box::new(r.as_ref().as_ref().to_vec()));
    }
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E> {
        let mut l = self.lock();
        return l.write(a, x);
    }
    fn size(&self) -> Result<u64, E> {
        let l = self.lock();
        return l.size();
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        let mut l = self.lock();
        return l.grow(x);
    }
}
impl<T: Memory<E>, E> Memory<E> for Arc<spin::RwLock<T>> {
    fn read2(
        &self,
        a: u64,
        s: u64,
        handle: &mut (dyn FnMut(&[u8]) -> Result<(), E> + '_),
    ) -> Result<(), E> {
        let l = spin::RwLock::read(&self);
        return l.read2(a, s, handle);
    }
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        let l = spin::RwLock::read(&self);
        let r = l.read(a, s)?;
        return Ok(Box::new(r.as_ref().as_ref().to_vec()));
    }
    fn write(&mut self, a: u64, x: &[u8]) -> Result<(), E> {
        let mut l = spin::RwLock::write(&self);
        return l.write(a, x);
    }
    fn size(&self) -> Result<u64, E> {
        let l = spin::RwLock::read(&self);
        return l.size();
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        let mut l = spin::RwLock::write(&self);
        return l.grow(x);
    }
}
// pub unsafe fn host_memory() -> impl Memory {
//     struct W {}
//     impl Memory for W {
//         fn read<'a>(&'a self, a: u64, s: u64) -> anyhow::Result<Box<dyn AsRef<[u8]> + 'a>> {
//             return Ok(Box::new(unsafe {
//                 core::slice::from_raw_parts(a as usize as *const u8, s as usize)
//             }));
//         }
//         fn write(&mut self, a: u64, x: &[u8]) -> anyhow::Result<()> {
//             let n = unsafe { core::slice::from_raw_parts_mut(a as usize as *mut u8, x.len()) };
//             n.copy_from_slice(x);
//             return Ok(());
//         }
//         fn size(&self) -> Result<u64, anyhow::Error> {
//             anyhow::bail!("host memory cannot use size")
//         }
//         fn grow(&mut self, x: u64) -> anyhow::Result<()> {
//             anyhow::bail!("host memory cannot use grow")
//         }
//     }
//     return W {};
// }
pub mod _rexport {
    pub use anyhow;
    pub use tramp;
    pub use tuple_list;
    pub extern crate alloc;
    pub use core;
    pub use core::error::Error;
}
macro_rules! int_ty{
    ($int:ty => $p:ident) => {
        paste::paste!{
            pub fn [<$p add>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a.wrapping_add(b)))
            }
            pub fn [<$p mul>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a.wrapping_mul(b)))
            }
            pub fn [<$p and>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a & b))
            }
            pub fn [<$p or>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a | b))
            }
            pub fn [<$p xor>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a ^ b))
            }
            pub fn [<$p shl>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a << b))
            }
            pub fn [<$p shru>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a >> b))
            }
            pub fn [<$p shrs>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(((a as $p) >> b) as $int))
            }
            pub fn [<$p divu>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a / b))
            }
            pub fn [<$p divs>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(((a as $p) / (b as $p)) as $int))
            }
            pub fn [<$p remu>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a % b))
            }
            pub fn [<$p rems>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(((a as $p) % (b as $p)) as $int))
            }
            pub fn [<$p rotl>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a.rotate_left((b & 0xffffffff) as u32)))
            }
            pub fn [<$p clz>]<E>(a: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a.leading_zeros() as $int))
            }
            pub fn [<$p ctz>]<E>(a: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a.trailing_zeros() as $int))
            }
            //comparisons
            pub fn [<$p eqz>]<E>(a: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a == 0{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p eq>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a == b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p ne>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a != b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p ltu>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a < b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p gtu>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a > b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p leu>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a <= b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p geu>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                Ok(tuple_list::tuple_list!(if a >= b{
                    1
                }else{
                    0
                }))
            }
            //signed
            pub fn [<$p lts>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                let a = a as $p;
                let b = b as $p;
                Ok(tuple_list::tuple_list!(if a < b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p gts>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                let a = a as $p;
                let b = b as $p;
                Ok(tuple_list::tuple_list!(if a > b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p les>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                let a = a as $p;
                let b = b as $p;
                Ok(tuple_list::tuple_list!(if a <= b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p ges>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!(u32), E> {
                let a = a as $p;
                let b = b as $p;
                Ok(tuple_list::tuple_list!(if a >= b{
                    1
                }else{
                    0
                }))
            }
            pub fn [<$p sub>]<E>(a: $int, b: $int) -> Result<tuple_list::tuple_list_type!($int), E> {
                Ok(tuple_list::tuple_list!(a.wrapping_sub(b)))
            }
            //LOADS and STORES
            pub fn [<$p load>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), core::mem::size_of::<$int>().try_into().unwrap(),&mut |a|{
                    r = $int::from_ne_bytes(a.try_into().unwrap());
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r))
            }
            pub fn [<$p store>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T, c: $int) -> Result<(), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                a.write(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), &c.to_ne_bytes())?;
                Ok(())
            }
            //8 BIT
            pub fn [<$p load8u>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), 1,&mut |a|{
                    r = a[0] as $int;
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r))
            }
            pub fn [<$p load8s>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), 1,&mut |a|{
                    r = a[0] as i8 as $p as $int;
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r))

            }
            pub fn [<$p store8>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T, c: $int) -> Result<(), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                a.write(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), &[(c & 0xff) as u8])?;
                Ok(())
            }
            //16 BIT
            pub fn [<$p load16u>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), 2,&mut |a|{
                    r = u16::from_ne_bytes(a.try_into().unwrap()) as $int;
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r))
            }
            pub fn [<$p load16s>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), 2,&mut |a|{
                    r = u16::from_ne_bytes(a.try_into().unwrap()) as i16 as $p as $int;
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r as i16 as $p as $int))
            }
            pub fn [<$p store16>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T, c: $int) -> Result<(), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                a.write(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), &((c & 0xffff) as u16).to_ne_bytes())?;
                Ok(())
            }
            //32 BIT
            pub fn [<$p load32u>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), 4,&mut |a|{
                    r = u32::from_ne_bytes(a.try_into().unwrap()) as $int;
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r))
            }
            pub fn [<$p load32s>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T) -> Result<tuple_list::tuple_list_type!($int), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                let mut r = 0;
                 a.read2(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), 4,&mut |a|{
                    r = u32::from_ne_bytes(a.try_into().unwrap()) as i32 as $p as $int;
                    Ok(())
                 })?;
                Ok(tuple_list::tuple_list!(r as i32 as $p as $int))
            }
            pub fn [<$p store32>]<T: TryInto<u64>, M: Memory<E> + ?Sized,E>(a: &mut M, b: T, c: $int) -> Result<(), E> where T::Error: Into<anyhow::Error> + Send + Sync + 'static {
                a.write(b.try_into()/* .map_err(|e| E::from(e.into()))? */ .map_err(|_|()).expect("to work"), &((c & 0xffffffff) as u32).to_ne_bytes())?;
                Ok(())
            }
        }
    }
}
int_ty!(u32 => i32);
int_ty!(u64 => i64);
pub fn select<T, E>(u: u32, t: T, t2: T) -> Result<tuple_list::tuple_list_type!(T), E> {
    Ok(tuple_list::tuple_list!(if u != 0 { t } else { t2 }))
}
pub fn i32wrapi64<E>(a: u64) -> Result<tuple_list::tuple_list_type!(u32), E> {
    return Ok(tuple_list::tuple_list!((a & 0xffffffff) as u32));
}
pub fn i64extendi32u<E>(a: u32) -> Result<tuple_list::tuple_list_type!(u64), E> {
    Ok(tuple_list::tuple_list!(a as u64))
}
pub fn i64extendi32s<E>(a: u32) -> Result<tuple_list::tuple_list_type!(u64), E> {
    Ok(tuple_list::tuple_list!(a as i32 as i64 as u64))
}
pub fn i64truncf64s<E>(a: f64) -> Result<tuple_list::tuple_list_type!(u64), E> {
    Ok(tuple_list::tuple_list!(
        unsafe { a.trunc().to_int_unchecked::<i64>() } as u64
    ))
}
