
use std::cell::UnsafeCell;
use std::future::Future;
#[cfg(not(feature = "loom"))]
use std::hint::spin_loop;
use std::marker::PhantomPinned;
use std::mem::align_of;
use std::pin::Pin;
use std::ptr::{from_ref, null, null_mut};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicPtr, AtomicU16};
use std::task::{Context, Poll, Waker};
#[cfg(not(feature = "loom"))]
use std::thread::{Thread, current, park, yield_now};
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

#[cfg(feature = "loom")]
use loom::hint::spin_loop;
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicPtr, AtomicU16};
#[cfg(feature = "loom")]
use loom::thread::{Thread, current, park, yield_now};

use crate::opcode::Opcode;
use crate::sync_primitive::SyncPrimitive;

#[derive(Default)]
#[repr(align(8))]
pub(crate) struct WaitQueue {
    #[cfg(not(feature = "loom"))]
    raw_data: UnsafeCell<[u64; 16]>,
    #[cfg(feature = "loom")] // Loom types are larger than those in the standard library.
    raw_data: UnsafeCell<[[u64; 16]; 32]>,
    _pinned: PhantomPinned,
}

#[repr(align(8))]
pub(crate) struct Entry {
    next_entry_anchor_ptr: AtomicPtr<u64>,
    prev_entry_ptr: AtomicPtr<Self>,
    opcode: Opcode,
    state: AtomicU16,
    pollable: std::sync::atomic::AtomicBool, // `Loom` is too slow when it is added to modeling.
    monitor: Monitor,
    drop_callback: fn(*const Self),
    addr: AtomicPtr<()>,
    offset: u16,
}

enum Monitor {
    Async(UnsafeCell<Option<Waker>>),
    Sync(UnsafeCell<Option<Thread>>),
}

const _WAIT_QUEUE_ALIGN_ASSERT: () = assert!(align_of::<WaitQueue>() == 8);
const _ENTRY_ALIGN_ASSERT: () = assert!(align_of::<Entry>() == 8);
const _ENTRY_SIZE_ASSERT: () = assert!(size_of::<Entry>() <= WaitQueue::VIRTUAL_ALIGNMENT / 2);

impl WaitQueue {
    #[cfg(not(feature = "loom"))]
    pub(crate) const SPIN_DURATION: Duration = Duration::from_micros(32);
    #[cfg(feature = "loom")]
    pub(crate) const SPIN_DURATION: Duration = Duration::ZERO;

    #[cfg(not(feature = "loom"))]
    pub(crate) const VIRTUAL_ALIGNMENT: usize = 128;
    #[cfg(feature = "loom")]
    pub(crate) const VIRTUAL_ALIGNMENT: usize = 4096;

    #[cfg(not(feature = "loom"))]
    pub(crate) const LOCKED_FLAG: usize = Self::VIRTUAL_ALIGNMENT >> 1;
    #[cfg(feature = "loom")]
    pub(crate) const LOCKED_FLAG: usize = 64;

    pub(crate) const DATA_MASK: usize = Self::LOCKED_FLAG - 1;

    pub(crate) const ADDR_MASK: usize = !(Self::LOCKED_FLAG | Self::DATA_MASK);

    pub(crate) fn construct<S: SyncPrimitive>(
        self: Pin<&Self>,
        sync_primitive: &S,
        opcode: Opcode,
        is_sync: bool,
    ) {
        let (anchor_ptr, offset) = self.anchor_ptr();
        let had_entry = unsafe {
            if *anchor_ptr == 0 {
                *anchor_ptr.cast_mut() = u64::MAX - u64::try_from(offset).unwrap_or(0);
                false
            } else {
                debug_assert_eq!(*anchor_ptr, u64::MAX - u64::try_from(offset).unwrap_or(0));
                true
            }
        };
        let entry_ptr = Self::to_entry_ptr(anchor_ptr).cast_mut();
        let monitor = if is_sync {
            Monitor::Sync(UnsafeCell::new(None))
        } else {
            Monitor::Async(UnsafeCell::new(None))
        };
        unsafe {
            if had_entry {
                debug_assert_eq!((*entry_ptr).state.load(Relaxed), 0);
                debug_assert!(!(*entry_ptr).pollable.load(Relaxed));
                (*entry_ptr).prev_entry_ptr.store(null_mut(), Relaxed);
                (*entry_ptr).opcode = opcode;
                (*entry_ptr).monitor = monitor;
                (*entry_ptr)
                    .addr
                    .store(from_ref(sync_primitive).cast::<()>().cast_mut(), Release);
            } else {
                let entry = Entry {
                    next_entry_anchor_ptr: AtomicPtr::new(null_mut()),
                    prev_entry_ptr: AtomicPtr::new(null_mut()),
                    opcode,
                    state: AtomicU16::new(0),
                    pollable: std::sync::atomic::AtomicBool::new(false),
                    monitor,
                    drop_callback: S::drop_wait_queue_entry,
                    addr: AtomicPtr::new(from_ref(sync_primitive).cast::<()>().cast_mut()),
                    offset: u16::try_from(entry_ptr.addr() - self.raw_data.get().addr())
                        .unwrap_or(0),
                };
                entry_ptr.write(entry);
            }
        }
    }

    #[inline]
    pub(crate) fn is_pollable(&self) -> bool {
        let entry_ptr = Self::to_entry_ptr(self.anchor_ptr().0);
        if entry_ptr.is_null() {
            false
        } else {
            Entry::entry_ref(entry_ptr).pollable.load(Acquire)
        }
    }

    #[inline]
    pub(crate) const fn pin(&self) -> Pin<&WaitQueue> {
        unsafe { Pin::new_unchecked(self) }
    }

    #[inline]
    pub(crate) const fn pin_ptr<'l>(wait_queue_ptr: *const WaitQueue) -> Pin<&'l WaitQueue> {
        unsafe { Pin::new_unchecked(&*wait_queue_ptr) }
    }

    #[inline]
    pub(crate) fn entry_ptr(&self) -> *const Entry {
        Self::to_entry_ptr(self.anchor_ptr().0)
    }

    #[inline]
    pub(crate) fn to_entry_ptr(anchor_ptr: *const u64) -> *const Entry {
        let anchor_val = unsafe { *anchor_ptr };
        if anchor_val == 0 {
            return null();
        }

        anchor_ptr
            .map_addr(|addr| {
                debug_assert_eq!(addr % Self::VIRTUAL_ALIGNMENT, 0);

                let offset = usize::try_from(u64::MAX - anchor_val).unwrap_or(0);
                let start_addr = addr - offset;
                debug_assert_eq!(start_addr % 8, 0);

                if offset < Self::VIRTUAL_ALIGNMENT / 2 {
                    start_addr + Self::VIRTUAL_ALIGNMENT / 2
                } else {
                    start_addr
                }
            })
            .cast::<Entry>()
    }

    #[inline]
    pub(crate) fn to_anchor_ptr(state: *mut ()) -> *const u64 {
        state.map_addr(|addr| addr & Self::ADDR_MASK).cast::<u64>()
    }

    #[inline]
    pub(crate) fn anchor_ptr(&self) -> (*const u64, usize) {
        let start_addr = self.raw_data.get();
        let mut offset = 0;
        let anchor_ptr = start_addr
            .map_addr(|addr| {
                let anchor_addr = if addr % Self::VIRTUAL_ALIGNMENT == 0 {
                    addr
                } else {
                    addr + Self::VIRTUAL_ALIGNMENT - (addr % Self::VIRTUAL_ALIGNMENT)
                };
                debug_assert_eq!(addr % 8, 0);
                debug_assert_eq!(anchor_addr % Self::VIRTUAL_ALIGNMENT, 0);
                debug_assert!(anchor_addr - addr < Self::VIRTUAL_ALIGNMENT);
                offset = anchor_addr - addr;
                anchor_addr
            })
            .cast::<u64>();
        (anchor_ptr, offset)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn spin_until<R, P: FnMut() -> R, C: FnMut(&R) -> bool>(
        mut pred: P,
        mut check: C,
    ) -> R {
        let mut spin_count = 2;
        let mut timer_start: Option<Instant> = None;
        loop {
            for _ in 0..spin_count {
                spin_loop();
            }
            let result = pred();
            if check(&result)
                || timer_start
                    .as_ref()
                    .is_some_and(|t| t.elapsed() > Self::SPIN_DURATION)
                || cfg!(feature = "loom")
            {
                return result;
            }
            spin_count = (spin_count * 2).min(2048);
            if spin_count == 32 {
                timer_start.replace(Instant::now());
            }
        }
    }
}

impl Drop for WaitQueue {
    #[inline]
    fn drop(&mut self) {
        let anchor_ptr = self.anchor_ptr().0;
        let entry_ptr = Self::to_entry_ptr(anchor_ptr).cast_mut();
        if entry_ptr.is_null() {
            return;
        }

        unsafe {
            if (*entry_ptr).pollable.load(Acquire) {
                ((*entry_ptr).drop_callback)(entry_ptr);
                (*entry_ptr).pollable.store(false, Release);
            }
            entry_ptr.drop_in_place();
        }
    }
}

impl Future for Pin<&'_ WaitQueue> {
    type Output = u8;

    #[inline(never)] // The amount of code in the caller affects performance.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Entry::entry_ref(self.entry_ptr()).poll_result_async(cx)
    }
}

unsafe impl Send for WaitQueue {}
unsafe impl Sync for WaitQueue {}

impl Entry {
    pub(crate) const ERROR_WRONG_MODE: u8 = u8::MAX;

    const RESULT_SET: u16 = 1_u16 << u8::BITS;

    const WAKER_SET: u16 = 1_u16 << (u8::BITS + 1);

    const RESULT_FINALIZED: u16 = 1_u16 << (u8::BITS + 2);

    #[inline]
    pub(crate) fn entry_ref<'e>(entry_ptr: *const Entry) -> &'e Entry {
        unsafe { &(*entry_ptr) }
    }

    #[inline]
    pub(crate) fn to_wait_queue_ptr(entry_ptr: *const Self) -> *const WaitQueue {
        entry_ptr
            .map_addr(|addr| addr - usize::from(Self::entry_ref(entry_ptr).offset))
            .cast::<WaitQueue>()
    }

    #[inline]
    pub(crate) fn next_entry_anchor_ptr(&self) -> *const u64 {
        self.next_entry_anchor_ptr.load(Acquire)
    }

    #[inline]
    pub(crate) fn next_entry_ptr(&self) -> *const Self {
        let anchor_ptr = self.next_entry_anchor_ptr.load(Acquire);
        if anchor_ptr.is_null() {
            return null();
        }
        WaitQueue::to_entry_ptr(anchor_ptr)
    }

    #[inline]
    pub(crate) fn prev_entry_ptr(&self) -> *const Self {
        self.prev_entry_ptr.load(Acquire)
    }

    #[inline]
    pub(crate) fn update_next_entry_anchor_ptr(&self, next_entry_anchor_ptr: *const u64) {
        debug_assert_eq!(
            next_entry_anchor_ptr as usize % WaitQueue::VIRTUAL_ALIGNMENT,
            0
        );
        self.next_entry_anchor_ptr
            .store(next_entry_anchor_ptr.cast_mut(), Release);
    }

    #[inline]
    pub(crate) fn update_prev_entry_ptr(&self, prev_entry_ptr: *const Self) {
        self.prev_entry_ptr
            .store(prev_entry_ptr.cast_mut(), Release);
    }

    #[inline]
    pub(crate) const fn opcode(&self) -> Opcode {
        self.opcode
    }

    #[inline]
    pub(crate) fn sync_primitive_ref<S: SyncPrimitive>(&self) -> &S {
        unsafe { self.addr.load(Acquire).cast::<S>().as_ref().unwrap() }
    }

    pub(crate) fn set_prev_ptr(tail_entry_ptr: *const Self) {
        let mut entry_ptr = tail_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = {
                let next_entry_ptr = Self::entry_ref(entry_ptr).next_entry_ptr();
                if !next_entry_ptr.is_null() {
                    let next_entry = Self::entry_ref(next_entry_ptr);
                    if next_entry.prev_entry_ptr().is_null() {
                        next_entry.update_prev_entry_ptr(entry_ptr);
                    } else {
                        debug_assert_eq!(next_entry.prev_entry_ptr(), entry_ptr);
                        return;
                    }
                }
                next_entry_ptr
            };
        }
    }

    pub(crate) fn iter_forward<F: FnMut(*const Self, *const Self) -> bool>(
        tail_entry_ptr: *const Self,
        set_prev: bool,
        mut f: F,
    ) {
        let mut entry_ptr = tail_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = {
                let next_entry_ptr = Self::entry_ref(entry_ptr).next_entry_ptr();
                if set_prev && !next_entry_ptr.is_null() {
                    Self::entry_ref(next_entry_ptr).update_prev_entry_ptr(entry_ptr);
                }

                if f(entry_ptr, next_entry_ptr) {
                    return;
                }
                next_entry_ptr
            };
        }
    }

    pub(crate) fn iter_backward<F: FnMut(*const Self, *const Self) -> bool>(
        head_entry_ptr: *const Self,
        mut f: F,
    ) {
        let mut entry_ptr = head_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = {
                let prev_entry_ptr = Self::entry_ref(entry_ptr).prev_entry_ptr();
                if f(entry_ptr, prev_entry_ptr) {
                    return;
                }
                prev_entry_ptr
            };
        }
    }

    #[inline]
    pub(crate) fn set_result(entry_ptr: *const Self, result: u8) {
        unsafe {
            (*entry_ptr).pollable.store(true, Release);
            let mut state = (*entry_ptr).state.load(Acquire);
            loop {
                debug_assert_eq!(state & Self::RESULT_SET, 0);
                debug_assert_eq!(state & Self::RESULT_FINALIZED, 0);

                match (*entry_ptr).state.compare_exchange_weak(
                    state,
                    (state | Self::RESULT_SET) | u16::from(result),
                    AcqRel,
                    Acquire,
                ) {
                    Ok(_) => break,
                    Err(new_state) => state = new_state,
                }
            }

            if state & Self::WAKER_SET == Self::WAKER_SET {
                match &(*entry_ptr).monitor {
                    Monitor::Async(waker) => {
                        if let Some(waker) = (*waker.get()).take() {
                            (*entry_ptr).state.fetch_or(Self::RESULT_FINALIZED, AcqRel);
                            waker.wake();
                        }
                    }
                    Monitor::Sync(thread) => {
                        if let Some(thread) = (*thread.get()).take() {
                            (*entry_ptr).state.fetch_or(Self::RESULT_FINALIZED, AcqRel);

                            thread.unpark();
                        }
                    }
                }
            } else {
                (*entry_ptr).state.fetch_or(Self::RESULT_FINALIZED, AcqRel);
            }
        }
    }

    pub(crate) fn poll_result_sync(&self) -> u8 {
        let Monitor::Sync(thread) = &self.monitor else {
            return Self::ERROR_WRONG_MODE;
        };

        loop {
            if let Some(result) = self.try_consume_result() {
                return result;
            }
            if let Some(result) =
                WaitQueue::spin_until(|| self.try_consume_result(), Option::is_some)
            {
                return result;
            }

            let state = self.state.load(Acquire);
            if state & Self::RESULT_SET == Self::RESULT_SET {
                continue;
            }

            if state & Self::WAKER_SET == Self::WAKER_SET
                && self
                    .state
                    .compare_exchange_weak(state, state & !Self::WAKER_SET, AcqRel, Acquire)
                    .is_err()
            {
                continue;
            }

            unsafe {
                (*thread.get()).replace(current());
            }
            if (self.state.fetch_or(Self::WAKER_SET, Release) & Self::RESULT_SET)
                != Self::RESULT_SET
            {
                park();
            }
        }
    }

    #[inline]
    pub(crate) fn set_pollable(&self) {
        self.pollable.store(true, Release);
    }

    #[inline]
    pub(crate) fn result_finalized(&self) -> bool {
        let state = self.state.load(Acquire);
        state & Self::RESULT_FINALIZED == Self::RESULT_FINALIZED
    }

    #[inline]
    pub(crate) fn acknowledge_result_sync(&self) -> u8 {
        loop {
            if let Some(result) = self.try_consume_result() {
                return result;
            }
            yield_now();
        }
    }

    #[inline]
    pub(crate) fn try_consume_result(&self) -> Option<u8> {
        let state = self.state.load(Acquire);
        if state & Self::RESULT_FINALIZED == Self::RESULT_FINALIZED {
            debug_assert_ne!(state & Self::RESULT_SET, 0);
            self.state.store(0, Release);
            self.pollable.store(false, Release);
            return u8::try_from(state & ((1_u16 << u8::BITS) - 1)).ok();
        }
        None
    }

    #[inline]
    fn poll_result_async(&self, cx: &mut Context<'_>) -> Poll<u8> {
        let Monitor::Async(waker) = &self.monitor else {
            return Poll::Ready(Self::ERROR_WRONG_MODE);
        };

        if let Some(result) = self.try_consume_result() {
            return Poll::Ready(result);
        }
        if let Some(result) = WaitQueue::spin_until(|| self.try_consume_result(), Option::is_some) {
            return Poll::Ready(result);
        }

        let state = self.state.load(Acquire);
        if state & Self::RESULT_SET == Self::RESULT_SET {
            if let Some(result) = self.try_consume_result() {
                return Poll::Ready(result);
            }
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        if (state & Self::WAKER_SET) == Self::WAKER_SET
            && self
                .state
                .compare_exchange_weak(state, state & !Self::WAKER_SET, AcqRel, Acquire)
                .is_err()
        {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        unsafe {
            (*waker.get()).replace(cx.waker().clone());
        }
        if (self.state.fetch_or(Self::WAKER_SET, Release) & Self::RESULT_SET) == Self::RESULT_SET {
            cx.waker().wake_by_ref();
        }

        Poll::Pending
    }
}

unsafe impl Send for Monitor {}

unsafe impl Sync for Monitor {}
