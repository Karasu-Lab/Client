//! [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.

use std::fmt;
use std::pin::Pin;
use std::ptr::{null_mut, without_provenance_mut};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicPtr;

use crate::Pager;
use crate::opcode::Opcode;
use crate::pager::{self, SyncResult};
use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::{Entry, WaitQueue};

/// [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.
///
/// The locking semantics are similar to [`RwLock`](std::sync::RwLock), however, [`Lock`] only
/// provides low-level locking and releasing methods, hence forcing the user to manage the scope of
/// acquired locks and the resources to protect.
#[derive(Default)]
pub struct Lock {
    /// [`Lock`] state.
    state: AtomicPtr<()>,
}

/// An enumeration of possible errors which can occur while trying to acquire a lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TryLockError {
    /// The lock could not be acquired because it was poisoned.
    Poisoned,
    /// The lock could not be acquired at this time because the operation would otherwise block.
    WouldBlock,
}

/// Operation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Acquires an exclusive lock.
    Exclusive,
    /// Acquires a shared lock.
    Shared,
    /// Waits for the [`Lock`] to be free or poisoned.
    ///
    /// [`Self::WaitExclusive`], [`Self::WaitShared`], [`Lock::try_lock`], and [`Lock::try_share`]
    /// provide a way to bypass the fair queuing mechanism without spinning.
    WaitExclusive,
    /// Waits for a shared lock to be available or the [`Lock`] to be poisoned.
    ///
    /// If a [`Self::WaitExclusive`] entry is in front of a [`Self::WaitShared`] entry, the
    /// [`Self::WaitShared`] entry has to wait until the [`Self::WaitExclusive`] entry is processed.
    WaitShared,
}

impl Lock {
    /// Maximum number of shared owners.
    pub const MAX_SHARED_OWNERS: usize = WaitQueue::DATA_MASK - 1;

    /// Poisoned state.
    const POISONED_STATE: usize = WaitQueue::LOCKED_FLAG;

    /// Successfully acquired the desired lock.
    const ACQUIRED: u8 = 0_u8;

    /// Failed to acquire the desired lock.
    const NOT_ACQUIRED: u8 = 1_u8;

    /// Poisoned error code.
    const POISONED: u8 = 2_u8;

    /// Creates a new [`Lock`].
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    ///
    /// let lock = Lock::new();
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicPtr::new(null_mut()),
        }
    }

    /// Creates a new [`Lock`].
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicPtr::new(null_mut()),
        }
    }

    /// Returns `true` if the lock is currently free.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    /// assert!(lock.is_free(Relaxed));
    ///
    /// lock.lock_sync();
    /// assert!(!lock.is_free(Relaxed));
    /// ```
    #[inline]
    pub fn is_free(&self, mo: Ordering) -> bool {
        let state = self.state.load(mo).addr();
        state != Self::POISONED_STATE && (state & WaitQueue::DATA_MASK) == 0
    }

    /// Returns `true` if an exclusive lock is currently held.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    /// assert!(!lock.is_locked(Relaxed));
    ///
    /// lock.lock_sync();
    /// assert!(lock.is_locked(Relaxed));
    /// assert!(!lock.is_shared(Relaxed));
    /// ```
    #[inline]
    pub fn is_locked(&self, mo: Ordering) -> bool {
        (self.state.load(mo).addr() & WaitQueue::DATA_MASK) == WaitQueue::DATA_MASK
    }

    /// Returns `true` if shared locks are currently held.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(!lock.is_shared(Relaxed));
    ///
    /// lock.share_sync();
    /// assert!(lock.is_shared(Relaxed));
    /// assert!(!lock.is_locked(Relaxed));
    /// ```
    #[inline]
    pub fn is_shared(&self, mo: Ordering) -> bool {
        let share_state = self.state.load(mo).addr() & WaitQueue::DATA_MASK;
        share_state != 0 && share_state != WaitQueue::DATA_MASK
    }

    /// Returns `true` if the [`Lock`] is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    /// assert!(!lock.is_poisoned(Relaxed));
    ///
    /// lock.lock_sync();
    /// assert!(lock.poison_lock());
    /// assert!(lock.is_poisoned(Relaxed));
    /// ```
    #[inline]
    pub fn is_poisoned(&self, mo: Ordering) -> bool {
        self.state.load(mo).addr() == Self::POISONED_STATE
    }

    /// Acquires an exclusive lock asynchronously.
    ///
    /// Returns `false` if the lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// async {
    ///     lock.lock_async().await;
    ///     assert!(lock.is_locked(Relaxed));
    ///     assert!(!lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn lock_async(&self) -> bool {
        self.lock_async_with(|| ()).await
    }

    /// Acquires an exclusive lock synchronously.
    ///
    /// Returns `false` if the lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.is_locked(Relaxed));
    /// assert_eq!(lock.try_share(), Err(TryLockError::WouldBlock));
    /// ```
    #[inline]
    pub fn lock_sync(&self) -> bool {
        self.lock_sync_with(|| ())
    }

    /// Acquires an exclusive lock asynchronously with a wait callback.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// async {
    ///     lock.lock_async().await;
    ///     assert!(lock.is_locked(Relaxed));
    ///     assert!(!lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn lock_async_with<F: FnOnce()>(&self, begin_wait: F) -> bool {
        loop {
            let (mut result, state) = self.try_lock_internal_spin();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            let async_wait = WaitQueue::default();
            let async_wait_pinned = async_wait.pin();
            async_wait_pinned.construct(self, Opcode::Exclusive, false);
            if self.try_push_wait_queue_entry(async_wait_pinned, state, |state| {
                Self::resolve_state(state, Mode::Exclusive).0 == Self::NOT_ACQUIRED
            }) {
                begin_wait();
                result = async_wait_pinned.await;
                debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                return result == Self::ACQUIRED;
            }
        }
    }

    /// Acquires an exclusive lock synchronously with a wait callback.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.is_locked(Relaxed));
    /// assert_eq!(lock.try_share(), Err(TryLockError::WouldBlock));
    /// ```
    #[inline]
    pub fn lock_sync_with<F: FnOnce()>(&self, mut begin_wait: F) -> bool {
        loop {
            let (result, state) = self.try_lock_internal_spin();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            match self.wait_resources_sync(state, Opcode::Exclusive, begin_wait, |state| {
                Self::resolve_state(state, Mode::Exclusive).0 == Self::NOT_ACQUIRED
            }) {
                Ok(result) => {
                    debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                    return result == Self::ACQUIRED;
                }
                Err(returned) => begin_wait = returned,
            }
        }
    }

    /// Tries to acquire an exclusive lock.
    ///
    /// # Errors
    ///
    /// Returns a [`TryLockError`] if the lock is not free or poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    ///
    /// let lock = Lock::default();
    ///
    /// assert_eq!(lock.try_lock(), Ok(()));
    /// assert_eq!(lock.try_share(), Err(TryLockError::WouldBlock));
    /// assert_eq!(lock.try_lock(), Err(TryLockError::WouldBlock));
    ///
    /// assert!(lock.poison_lock());
    /// assert_eq!(lock.try_lock(), Err(TryLockError::Poisoned));
    /// ```
    #[inline]
    pub fn try_lock(&self) -> Result<(), TryLockError> {
        let result = self.try_lock_internal().0;
        if result == Self::ACQUIRED {
            Ok(())
        } else if result == Self::POISONED {
            Err(TryLockError::Poisoned)
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    /// Acquires a shared lock asynchronously.
    ///
    /// Returns `false` if the lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// async {
    ///     lock.share_async().await;
    ///     assert!(!lock.is_locked(Relaxed));
    ///     assert!(lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn share_async(&self) -> bool {
        self.share_async_with(|| ()).await
    }

    /// Acquires a shared lock synchronously.
    ///
    /// Returns `false` if the lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.share_sync();
    ///
    /// assert!(lock.is_shared(Relaxed));
    /// assert_eq!(lock.try_lock(), Err(TryLockError::WouldBlock));
    /// ```
    #[inline]
    pub fn share_sync(&self) -> bool {
        self.share_sync_with(|| ())
    }

    /// Acquires a shared lock asynchronously with a wait callback.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// async {
    ///     lock.share_async().await;
    ///     assert!(!lock.is_locked(Relaxed));
    ///     assert!(lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn share_async_with<F: FnOnce()>(&self, begin_wait: F) -> bool {
        loop {
            let (mut result, state) = self.try_share_internal_spin();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            let async_wait = WaitQueue::default();
            let async_wait_pinned = async_wait.pin();
            async_wait_pinned.construct(self, Opcode::Shared, false);
            if self.try_push_wait_queue_entry(async_wait_pinned, state, |state| {
                Self::resolve_state(state, Mode::Shared).0 == Self::NOT_ACQUIRED
            }) {
                begin_wait();
                result = async_wait_pinned.await;
                debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                return result == Self::ACQUIRED;
            }
        }
    }

    /// Acquires a shared lock synchronously with a wait callback.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.share_sync();
    ///
    /// assert!(lock.is_shared(Relaxed));
    /// assert_eq!(lock.try_lock(), Err(TryLockError::WouldBlock));
    /// ```
    #[inline]
    pub fn share_sync_with<F: FnOnce()>(&self, mut begin_wait: F) -> bool {
        loop {
            let (result, state) = self.try_share_internal_spin();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            match self.wait_resources_sync(state, Opcode::Shared, begin_wait, |state| {
                Self::resolve_state(state, Mode::Shared).0 == Self::NOT_ACQUIRED
            }) {
                Ok(result) => {
                    debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                    return result == Self::ACQUIRED;
                }
                Err(returned) => begin_wait = returned,
            }
        }
    }

    /// Tries to acquire a shared lock.
    ///
    /// # Errors
    ///
    /// Returns a [`TryLockError`] if an exclusive lock is held, the number of shared owners has
    /// reached [`Self::MAX_SHARED_OWNERS`], or the lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(lock.try_share().is_ok());
    /// assert!(lock.try_share().is_ok());
    /// assert_eq!(lock.try_lock(), Err(TryLockError::WouldBlock));
    /// ```
    #[inline]
    pub fn try_share(&self) -> Result<(), TryLockError> {
        let result = self.try_share_internal().0;
        if result == Self::ACQUIRED {
            Ok(())
        } else if result == Self::POISONED {
            Err(TryLockError::Poisoned)
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    /// Registers a [`Pager`] to allow it to get an exclusive lock or a shared lock remotely.
    ///
    /// `is_sync` indicates whether the [`Pager`] will be polled asynchronously (`false`) or
    /// synchronously (`true`).
    ///
    /// Returns `false` if the [`Pager`] was already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Lock, Pager};
    /// use saa::lock::Mode;
    ///
    /// let lock = Lock::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(lock.register_pager(&mut pinned_pager, Mode::Exclusive, true));
    /// assert!(!lock.register_pager(&mut pinned_pager, Mode::Exclusive, true));
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(true));
    /// ```
    #[inline]
    pub fn register_pager<'l>(
        &'l self,
        pager: &mut Pin<&mut Pager<'l, Self>>,
        mode: Mode,
        is_sync: bool,
    ) -> bool {
        if pager.is_registered() {
            return false;
        }

        let opcode = match mode {
            Mode::Exclusive => Opcode::Exclusive,
            Mode::Shared => Opcode::Shared,
            Mode::WaitExclusive => Opcode::Wait(u8::try_from(WaitQueue::DATA_MASK).unwrap_or(0)),
            Mode::WaitShared => Opcode::Wait(1),
        };
        pager.wait_queue().construct(self, opcode, is_sync);

        loop {
            let (result, state) = match mode {
                Mode::Exclusive => self.try_lock_internal_spin(),
                Mode::Shared => self.try_share_internal_spin(),
                Mode::WaitExclusive | Mode::WaitShared => self.try_wait_internal_spin(mode),
            };
            if result == Self::ACQUIRED || result == Self::POISONED {
                Entry::set_result(pager.wait_queue().entry_ptr(), result);
                break;
            }
            if self.try_push_wait_queue_entry(pager.wait_queue(), state, |state| {
                Self::resolve_state(state, mode).0 == Self::NOT_ACQUIRED
            }) {
                break;
            }
        }
        true
    }

    /// Releases an exclusive lock.
    ///
    /// Returns `true` if an exclusive lock was previously held and successfully released.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.release_lock());
    /// assert!(!lock.release_lock());
    ///
    /// assert!(lock.try_share().is_ok());
    /// assert!(!lock.release_lock());
    /// assert!(lock.release_share());
    ///
    /// lock.lock_sync();
    /// lock.poison_lock();
    ///
    /// assert!(!lock.release_lock());
    /// ```
    #[inline]
    pub fn release_lock(&self) -> bool {
        match self.state.compare_exchange(
            without_provenance_mut(WaitQueue::DATA_MASK),
            null_mut(),
            Release,
            Relaxed,
        ) {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Exclusive),
        }
    }

    /// Poisons the lock with an exclusive lock held.
    ///
    /// Returns `false` if an exclusive lock is not held, or the lock was already poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(!lock.poison_lock());
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.poison_lock());
    /// assert!(lock.is_poisoned(Relaxed));
    ///
    /// assert!(!lock.poison_lock());
    ///
    /// assert!(!lock.lock_sync());
    /// assert!(!lock.share_sync());
    /// assert!(!lock.release_lock());
    /// assert!(!lock.release_share());
    /// ```
    #[inline]
    pub fn poison_lock(&self) -> bool {
        match self.state.compare_exchange(
            without_provenance_mut(WaitQueue::DATA_MASK),
            without_provenance_mut(Self::POISONED_STATE),
            Release,
            Relaxed,
        ) {
            Ok(_) => true,
            Err(state) => self.poison_lock_internal(state),
        }
    }

    /// Clears poison from the lock.
    ///
    /// Returns `true` if the lock was successfully cleared.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(!lock.poison_lock());
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.poison_lock());
    /// assert!(lock.clear_poison());
    /// assert!(!lock.is_poisoned(Relaxed));
    /// ```
    #[inline]
    pub fn clear_poison(&self) -> bool {
        self.state
            .compare_exchange(
                without_provenance_mut(Self::POISONED_STATE),
                null_mut(),
                Release,
                Relaxed,
            )
            .is_ok()
    }

    /// Releases a shared lock.
    ///
    /// Returns `true` if a shared lock was previously held and successfully released.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::{Lock, TryLockError};
    ///
    /// let lock = Lock::default();
    ///
    /// lock.share_sync();
    /// lock.share_sync();
    ///
    /// assert!(lock.release_share());
    ///
    /// assert_eq!(lock.try_lock(), Err(TryLockError::WouldBlock));
    /// assert!(lock.release_share());
    ///
    /// assert!(!lock.release_share());
    /// assert!(lock.try_lock().is_ok());
    ///
    /// lock.poison_lock();
    ///
    /// assert!(!lock.release_share());
    /// ```
    #[inline]
    pub fn release_share(&self) -> bool {
        let state = self.state.load(Relaxed);
        if state.addr() != 0
            && state.addr() <= Self::MAX_SHARED_OWNERS
            && self
                .state
                .compare_exchange_weak(state, state.map_addr(|addr| addr - 1), Release, Relaxed)
                .is_ok()
        {
            return true;
        }
        self.release_loop(self.state.load(Relaxed), Opcode::Shared)
    }

    /// Tries to acquire an exclusive lock with spin-backoff.
    #[inline]
    fn try_lock_internal_spin(&self) -> (u8, *mut ()) {
        if self
            .state
            .compare_exchange(
                null_mut(),
                without_provenance_mut(WaitQueue::DATA_MASK),
                Acquire,
                Acquire,
            )
            .is_ok()
        {
            return (Self::ACQUIRED, null_mut());
        }
        WaitQueue::spin_until(
            || self.try_lock_internal(),
            |(result, _)| *result != Self::NOT_ACQUIRED,
        )
    }

    /// Tries to acquire an exclusive lock.
    #[inline]
    fn try_lock_internal(&self) -> (u8, *mut ()) {
        let mut state = self.state.load(Acquire);
        loop {
            if state.addr() == Self::POISONED_STATE {
                return (Self::POISONED, state);
            } else if state.addr() != 0 {
                return (Self::NOT_ACQUIRED, state);
            }
            match self.state.compare_exchange(
                null_mut(),
                without_provenance_mut(WaitQueue::DATA_MASK),
                Acquire,
                Acquire,
            ) {
                Ok(_) => return (Self::ACQUIRED, null_mut()),
                Err(new_state) => state = new_state,
            }
        }
    }

    /// Tries to acquire a shared lock with spin-backoff.
    #[inline]
    fn try_share_internal_spin(&self) -> (u8, *mut ()) {
        let state = self.state.load(Acquire);
        if state.addr() < Self::MAX_SHARED_OWNERS
            && self
                .state
                .compare_exchange_weak(state, state.map_addr(|addr| addr + 1), Acquire, Acquire)
                .is_ok()
        {
            return (Self::ACQUIRED, null_mut());
        }
        WaitQueue::spin_until(
            || self.try_share_internal(),
            |(result, _)| *result != Self::NOT_ACQUIRED,
        )
    }

    /// Tries to acquire a shared lock.
    #[inline]
    fn try_share_internal(&self) -> (u8, *mut ()) {
        let mut state = self.state.load(Acquire);
        loop {
            if state.addr() == Self::POISONED_STATE {
                return (Self::POISONED, state);
            } else if state.addr() >= Self::MAX_SHARED_OWNERS {
                return (Self::NOT_ACQUIRED, state);
            }
            match self.state.compare_exchange(
                state,
                state.map_addr(|addr| addr + 1),
                Acquire,
                Acquire,
            ) {
                Ok(_) => return (Self::ACQUIRED, null_mut()),
                Err(new_state) => state = new_state,
            }
        }
    }

    /// Tries to wait for a lock to be in a desired state with spin-backoff.
    #[inline]
    fn try_wait_internal_spin(&self, mode: Mode) -> (u8, *mut ()) {
        let (result, state) = Self::resolve_state(self.state.load(Acquire), mode);
        if result != Self::NOT_ACQUIRED {
            return (result, state);
        }

        WaitQueue::spin_until(
            || Self::resolve_state(self.state.load(Acquire), mode),
            |(result, _)| *result != Self::NOT_ACQUIRED,
        )
    }

    /// Poisons the lock.
    fn poison_lock_internal(&self, mut state: *mut ()) -> bool {
        loop {
            if state.addr() == Self::POISONED_STATE
                || state.addr() & WaitQueue::DATA_MASK != WaitQueue::DATA_MASK
            {
                // Already poisoned or the lock is not in the exclusive locked state.
                return false;
            }
            if state.addr() & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                // This only happens when an asynchronous task is being cancelled.
                state = WaitQueue::spin_until(
                    || {
                        let state = self.state.load(Acquire);
                        let result =
                            if state.addr() & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                                Self::NOT_ACQUIRED
                            } else {
                                Self::ACQUIRED
                            };
                        (result, state)
                    },
                    |(result, _)| *result != Self::NOT_ACQUIRED,
                )
                .1;
                continue;
            }

            match self.state.compare_exchange(
                state,
                without_provenance_mut(Self::POISONED_STATE),
                AcqRel,
                Relaxed,
            ) {
                Ok(prev_state) => {
                    // A possible data race where the lock is poisoned while another task is still
                    // processing the wait queue is prevented by the wait queue processing logic;
                    // `models.rs` proves this.
                    debug_assert_eq!(prev_state.addr() & WaitQueue::LOCKED_FLAG, 0);
                    let anchor_ptr = WaitQueue::to_anchor_ptr(prev_state);
                    if !anchor_ptr.is_null() {
                        let tail_entry_ptr = WaitQueue::to_entry_ptr(anchor_ptr);
                        Entry::iter_forward(tail_entry_ptr, false, |entry_ptr, _| {
                            Entry::set_result(entry_ptr, Self::POISONED);
                            false
                        });
                    }

                    return true;
                }
                Err(new_state) => state = new_state,
            }
        }
    }

    /// Resolves the supplied state into a locking result.
    #[inline]
    fn resolve_state(state: *mut (), mode: Mode) -> (u8, *mut ()) {
        match mode {
            Mode::Exclusive | Mode::WaitExclusive if state.addr() == 0 => (Self::ACQUIRED, state),
            Mode::Shared | Mode::WaitShared if state.addr() < Self::MAX_SHARED_OWNERS => {
                (Self::ACQUIRED, state)
            }
            _ => {
                if state.addr() == Self::POISONED_STATE {
                    (Self::POISONED, state)
                } else {
                    (Self::NOT_ACQUIRED, state)
                }
            }
        }
    }
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Relaxed);
        let lock_share_state = state.addr() & WaitQueue::DATA_MASK;
        let locked = lock_share_state == WaitQueue::DATA_MASK;
        let share_count = if locked { 0 } else { lock_share_state };
        let poisoned = state.addr() == Self::POISONED_STATE;
        let wait_queue_being_processed =
            state.addr() & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG;
        let wait_queue_tail_addr = state.addr() & WaitQueue::ADDR_MASK;
        f.debug_struct("WaitQueue")
            .field("state", &state)
            .field("locked", &locked)
            .field("share_count", &share_count)
            .field("poisoned", &poisoned)
            .field("wait_queue_being_processed", &wait_queue_being_processed)
            .field("wait_queue_tail_addr", &wait_queue_tail_addr)
            .finish()
    }
}

impl SyncPrimitive for Lock {
    #[inline]
    fn state(&self) -> &AtomicPtr<()> {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        Self::MAX_SHARED_OWNERS
    }

    #[inline]
    fn drop_wait_queue_entry(entry_ptr: *const Entry) {
        Self::force_remove_wait_queue_entry(entry_ptr);
    }
}

impl SyncResult for Lock {
    type Result = Result<bool, pager::Error>;

    #[inline]
    fn to_result(value: u8, pager_error: Option<pager::Error>) -> Self::Result {
        pager_error.map_or_else(
            || {
                debug_assert!(value == Self::ACQUIRED || value == Self::POISONED);
                Ok(value == Self::ACQUIRED)
            },
            Err,
        )
    }
}
