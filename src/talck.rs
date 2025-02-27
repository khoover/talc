//! Home of Talck, a mutex-locked wrapper of Talc.

use crate::{talc::Talc, OomHandler};

use core::{
    alloc::{GlobalAlloc, Layout},
    cmp::Ordering,
    ptr::{NonNull, null_mut},
};

#[cfg(feature = "allocator")]
use core::alloc::AllocError;

#[cfg(feature = "allocator")]
pub(crate) fn is_aligned_to(ptr: *mut u8, align: usize) -> bool {
    (ptr as usize).trailing_zeros() >= align.trailing_zeros()
}

/// Talc lock, contains a mutex-locked [`Talc`].
///
/// # Example
/// ```rust
/// # use talc::*;
/// let talc = Talc::new(ErrOnOom);
/// let talck = talc.lock::<spin::Mutex<()>>();
/// ```
// #[derive(Debug)] TODO
pub struct Talck<R: lock_api::RawMutex, O: OomHandler> {
    mutex: lock_api::Mutex<R, Talc<O>>
}

impl<R: lock_api::RawMutex, O: OomHandler> Talck<R, O> {
    /// Create a new `Talck`.
    pub const fn new(talc: Talc<O>) -> Self {
        Self {
            mutex: lock_api::Mutex::new(talc),
        }
    }

    /// Lock the mutex and access the inner `Talc`.
    pub fn lock(&self) -> lock_api::MutexGuard<R, Talc<O>> {
        self.mutex.lock()
    }

    /// Try to lock the mutex and access the inner `Talc`.
    pub fn try_lock(&self) -> Option<lock_api::MutexGuard<R, Talc<O>>> {
        self.mutex.try_lock()
    }

    /// Retrieve the inner `Talc`.
    pub fn into_inner(self) -> Talc<O> {
        self.mutex.into_inner()
    }
}

unsafe impl<R: lock_api::RawMutex, O: OomHandler> GlobalAlloc for Talck<R, O> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.lock().malloc(layout).map_or(null_mut(), |nn| nn.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.lock().free(NonNull::new_unchecked(ptr), layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        match layout.size().cmp(&new_size) {
            Ordering::Less => self
                .lock()
                .grow(NonNull::new_unchecked(ptr), layout, new_size)
                .map_or(null_mut(), |nn| nn.as_ptr()),

            Ordering::Greater => {
                self.lock().shrink(NonNull::new_unchecked(ptr), layout, new_size);
                ptr
            }

            Ordering::Equal => ptr,
        }
    }
}

#[cfg(feature = "allocator")]
unsafe impl<R: lock_api::RawMutex, O: OomHandler> core::alloc::Allocator for Talck<R, O> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        if layout.size() == 0 {
            return Ok(NonNull::slice_from_raw_parts(NonNull::dangling(), 0));
        }

        unsafe { self.lock().malloc(layout) }
            .map(|nn| NonNull::slice_from_raw_parts(nn, layout.size()))
            .map_err(|_| AllocError)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            self.lock().free(ptr, layout);
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(new_layout.size() >= old_layout.size());

        if old_layout.size() == 0 {
            return self.allocate(new_layout);
        } else if !is_aligned_to(ptr.as_ptr(), new_layout.align()) {
            let mut talc = self.lock();
            let allocation = talc.malloc(new_layout).map_err(|_| AllocError)?;
            allocation.as_ptr().copy_from_nonoverlapping(ptr.as_ptr(), new_layout.size());
            talc.free(ptr, old_layout);
            Ok(NonNull::slice_from_raw_parts(allocation, new_layout.size()))
        } else {
            self.mutex
                .lock()
                .grow(ptr, old_layout, new_layout.size())
                .map(|nn| NonNull::slice_from_raw_parts(nn, new_layout.size()))
                .map_err(|_| AllocError)
        }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        let res = self.grow(ptr, old_layout, new_layout);

        if let Ok(allocation) = res {
            (allocation.as_ptr() as *mut u8)
                .add(old_layout.size())
                .write_bytes(0, new_layout.size() - old_layout.size());
        }

        res
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(new_layout.size() <= old_layout.size());

        if new_layout.size() == 0 {
            if old_layout.size() > 0 {
                self.lock().free(ptr, old_layout);
            }

            return Ok(NonNull::slice_from_raw_parts(NonNull::dangling(), 0));
        }

        if !is_aligned_to(ptr.as_ptr(), new_layout.align()) {
            let allocation = self.lock().malloc(new_layout).map_err(|_| AllocError)?;
            allocation.as_ptr().copy_from_nonoverlapping(ptr.as_ptr(), new_layout.size());
            self.lock().free(ptr, old_layout);
            return Ok(NonNull::slice_from_raw_parts(allocation, new_layout.size()));
        }

        self.lock().shrink(ptr, old_layout, new_layout.size());

        Ok(NonNull::slice_from_raw_parts(ptr, new_layout.size()))
    }
}

impl<O: OomHandler> Talc<O> {
    /// Wrap in `Talck`, a mutex-locked wrapper struct using [`lock_api`].
    ///
    /// This implements the [`GlobalAlloc`](core::alloc::GlobalAlloc) trait and provides
    /// access to the [`Allocator`](core::alloc::Allocator) API.
    ///
    /// # Examples
    /// ```
    /// # use talc::*;
    /// # use core::alloc::{GlobalAlloc, Layout};
    /// use spin::Mutex;
    /// let talc = Talc::new(ErrOnOom);
    /// let talck = talc.lock::<Mutex<()>>();
    ///
    /// unsafe {
    ///     talck.alloc(Layout::from_size_align_unchecked(32, 4));
    /// }
    /// ```
    pub const fn lock<R: lock_api::RawMutex>(self) -> Talck<R, O> {
        Talck::new(self)
    }
}

#[cfg(all(target_family = "wasm"))]
impl TalckWasm {
    /// Create a [`Talck`] instance that takes control of WASM memory management.
    ///
    /// # Safety
    /// The runtime environment must be single-threaded WASM.
    ///
    /// Note: calls to memory.grow during use of the allocator is allowed.
    pub const unsafe fn new_global() -> Self {
        Talc::new(crate::WasmHandler::new()).lock()
    }
}

#[cfg(all(target_family = "wasm"))]
pub type TalckWasm = Talck<crate::locking::AssumeUnlockable, crate::WasmHandler>;
