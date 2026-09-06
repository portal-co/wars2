//! Runtime support for the spectest host module, used by generated code.
//! Kept in `wars-rt` (behind the `spectest` feature) so generated crates need
//! only depend on `wars-rt`.

use crate::Memory;
use alloc::vec;
use alloc::vec::Vec;
use alloc::boxed::Box;

/// Number of times any spectest print function was invoked.
#[cfg(feature = "std")]
static PRINT_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Host sink for all `spectest.print*` imports.
pub fn print_sink() {
    #[cfg(feature = "std")]
    {
        PRINT_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// The shared host memory imported by spec modules as `spectest.memory`
/// (limits 1..2 pages).
pub struct HostMemory {
    pages: u32,
}

impl HostMemory {
    pub fn new() -> Self {
        Self { pages: 1 }
    }
}

impl Default for HostMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Memory<E> for HostMemory {
    fn read<'a>(&'a self, a: u64, s: u64) -> Result<Box<dyn AsRef<[u8]> + 'a>, E> {
        Ok(Box::new(vec![0u8; s as usize]))
    }
    fn write(&mut self, _a: u64, _x: &[u8]) -> Result<(), E> {
        Ok(())
    }
    fn size(&self) -> Result<u64, E> {
        Ok(self.pages as u64 * 65536)
    }
    fn grow(&mut self, x: u64) -> Result<(), E> {
        let new_pages = self.pages + (x as u32 + 65535) / 65536;
        if new_pages > 2 {
            return Ok(()); // Spec allows grow to fail; report unchanged size.
        }
        self.pages = new_pages;
        Ok(())
    }
}
