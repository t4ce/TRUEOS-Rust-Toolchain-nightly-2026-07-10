//! On some targets like wasm there's no threads, so no need to generate
//! thread locals and we can instead just use plain statics!

#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
use crate::cell::{Cell, UnsafeCell};
#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
use crate::mem::MaybeUninit;
#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
use crate::{
    boxed::Box,
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};
use crate::ptr;

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
const TRUEOS_STD_NO_THREADS_PER_SLOT: usize = 4096;

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
unsafe extern "C" {
    fn trueos_cabi_wls_current_slot() -> u32;
}

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
fn trueos_std_thread_local_slot() -> usize {
    let slot = unsafe { trueos_cabi_wls_current_slot() } as usize;
    if slot < TRUEOS_STD_NO_THREADS_PER_SLOT {
        slot
    } else {
        0
    }
}

#[cfg(all(
    target_has_threads,
    not(any(target_os = "trueos", target_os = "zkvm"))
))]
compile_error!("Using no_threads implementation on a target with threads");

#[doc(hidden)]
#[allow_internal_unstable(thread_local_internals)]
#[allow_internal_unsafe]
#[unstable(feature = "thread_local_internals", issue = "none")]
#[rustc_macro_transparency = "semiopaque"]
pub macro thread_local_inner {
    // used to generate the `LocalKey` value for const-initialized thread locals
    (@key $t:ty, $(#[$align_attr:meta])*, const $init:expr) => {{
        const __RUST_STD_INTERNAL_INIT: $t = $init;

        // NOTE: Please update the shadowing test in `tests/thread.rs` if these types are renamed.
        unsafe {
            $crate::thread::LocalKey::new(|_| {
                $(#[$align_attr])*
                static __RUST_STD_INTERNAL_VAL: $crate::thread::local_impl::LazyStorage<$t> =
                    $crate::thread::local_impl::LazyStorage::new();
                __RUST_STD_INTERNAL_VAL.get(None, || __RUST_STD_INTERNAL_INIT)
            })
        }
    }},

    // used to generate the `LocalKey` value for `thread_local!`
    (@key $t:ty, $(#[$align_attr:meta])*, $init:expr) => {{
        #[inline]
        fn __rust_std_internal_init_fn() -> $t { $init }

        unsafe {
            $crate::thread::LocalKey::new(|__rust_std_internal_init| {
                $(#[$align_attr])*
                static __RUST_STD_INTERNAL_VAL: $crate::thread::local_impl::LazyStorage<$t> = $crate::thread::local_impl::LazyStorage::new();
                __RUST_STD_INTERNAL_VAL.get(__rust_std_internal_init, __rust_std_internal_init_fn)
            })
        }
    }},
}

#[allow(missing_debug_implementations)]
#[repr(transparent)] // Required for correctness of `#[rustc_align_static]`
pub struct EagerStorage<T> {
    pub value: T,
}

// SAFETY: the target doesn't have threads.
unsafe impl<T> Sync for EagerStorage<T> {}

#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Initial,
    Alive,
    Destroying,
}

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
#[allow(missing_debug_implementations)]
pub struct LazyStorage<T> {
    slots: [AtomicUsize; TRUEOS_STD_NO_THREADS_PER_SLOT],
    _marker: PhantomData<fn() -> T>,
}

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
impl<T> LazyStorage<T> {
    pub const fn new() -> LazyStorage<T> {
        LazyStorage {
            slots: [const { AtomicUsize::new(0) }; TRUEOS_STD_NO_THREADS_PER_SLOT],
            _marker: PhantomData,
        }
    }

    #[inline]
    pub fn get(&'static self, i: Option<&mut Option<T>>, f: impl FnOnce() -> T) -> *const T {
        let slot = trueos_std_thread_local_slot();
        let cell = &self.slots[slot];
        let existing = cell.load(Ordering::Acquire);
        if existing != 0 {
            return existing as *const T;
        }

        self.initialize(cell, i, f)
    }

    #[cold]
    fn initialize(
        &'static self,
        cell: &AtomicUsize,
        i: Option<&mut Option<T>>,
        f: impl FnOnce() -> T,
    ) -> *const T {
        let value = i.and_then(Option::take).unwrap_or_else(f);
        let ptr = Box::leak(Box::new(value)) as *mut T as usize;
        match cell.compare_exchange(0, ptr, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => ptr as *const T,
            Err(existing) => {
                unsafe {
                    drop(Box::from_raw(ptr as *mut T));
                }
                existing as *const T
            }
        }
    }
}

#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
#[allow(missing_debug_implementations)]
#[repr(C)]
pub struct LazyStorage<T> {
    // This field must be first, for correctness of `#[rustc_align_static]`
    value: UnsafeCell<MaybeUninit<T>>,
    state: Cell<State>,
}

#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
impl<T> LazyStorage<T> {
    pub const fn new() -> LazyStorage<T> {
        LazyStorage {
            value: UnsafeCell::new(MaybeUninit::uninit()),
            state: Cell::new(State::Initial),
        }
    }

    /// Gets a pointer to the TLS value, potentially initializing it with the
    /// provided parameters.
    ///
    /// The resulting pointer may not be used after reentrant inialialization
    /// has occurred.
    #[inline]
    pub fn get(&'static self, i: Option<&mut Option<T>>, f: impl FnOnce() -> T) -> *const T {
        if self.state.get() == State::Alive {
            self.value.get() as *const T
        } else {
            self.initialize(i, f)
        }
    }

    #[cold]
    fn initialize(&'static self, i: Option<&mut Option<T>>, f: impl FnOnce() -> T) -> *const T {
        let value = i.and_then(Option::take).unwrap_or_else(f);

        // Destroy the old value if it is initialized
        // FIXME(#110897): maybe panic on recursive initialization.
        if self.state.get() == State::Alive {
            self.state.set(State::Destroying);
            // Safety: we check for no initialization during drop below
            unsafe {
                ptr::drop_in_place(self.value.get() as *mut T);
            }
            self.state.set(State::Initial);
        }

        // Guard against initialization during drop
        if self.state.get() == State::Destroying {
            panic!("Attempted to initialize thread-local while it is being dropped");
        }

        unsafe {
            self.value.get().write(MaybeUninit::new(value));
        }
        self.state.set(State::Alive);

        self.value.get() as *const T
    }
}

// SAFETY: the TRUEOS variant uses per-slot atomics and leaked values.
#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
unsafe impl<T> Sync for LazyStorage<T> {}

// SAFETY: the target doesn't have threads.
#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
unsafe impl<T> Sync for LazyStorage<T> {}

#[rustc_macro_transparency = "semiopaque"]
pub(crate) macro local_pointer {
    () => {},
    ($vis:vis static $name:ident; $($rest:tt)*) => {
        $vis static $name: $crate::sys::thread_local::LocalPointer = $crate::sys::thread_local::LocalPointer::__new();
        $crate::sys::thread_local::local_pointer! { $($rest)* }
    },
}

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
pub(crate) struct LocalPointer {
    slots: [AtomicUsize; TRUEOS_STD_NO_THREADS_PER_SLOT],
}

#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
impl LocalPointer {
    pub const fn __new() -> LocalPointer {
        LocalPointer {
            slots: [const { AtomicUsize::new(0) }; TRUEOS_STD_NO_THREADS_PER_SLOT],
        }
    }

    pub fn get(&self) -> *mut () {
        self.slots[trueos_std_thread_local_slot()].load(Ordering::Acquire) as *mut ()
    }

    pub fn set(&self, p: *mut ()) {
        self.slots[trueos_std_thread_local_slot()].store(p as usize, Ordering::Release)
    }
}

#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
pub(crate) struct LocalPointer {
    p: Cell<*mut ()>,
}

#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
impl LocalPointer {
    pub const fn __new() -> LocalPointer {
        LocalPointer { p: Cell::new(ptr::null_mut()) }
    }

    pub fn get(&self) -> *mut () {
        self.p.get()
    }

    pub fn set(&self, p: *mut ()) {
        self.p.set(p)
    }
}

// SAFETY: the TRUEOS variant uses per-slot atomics.
#[cfg(any(target_os = "trueos", target_os = "zkvm"))]
unsafe impl Sync for LocalPointer {}

// SAFETY: the target doesn't have threads.
#[cfg(not(any(target_os = "trueos", target_os = "zkvm")))]
unsafe impl Sync for LocalPointer {}
