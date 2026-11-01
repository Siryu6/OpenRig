//! Locks that outlive a panic. A thread that panics while holding a std lock
//! poisons it, and `lock().unwrap()` would then bring every other thread
//! down too. The panic is already logged (see `main`); the data behind the
//! lock is kept as the panicking thread left it, so the pedalboard goes on.

use std::sync::{
    Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::time::Duration;

pub trait MutexExt<T: ?Sized> {
    /// Lock, even after a panic of another holder
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> MutexExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub trait RwLockExt<T: ?Sized> {
    /// Read, even after a panic of a writer
    fn read_locked(&self) -> RwLockReadGuard<'_, T>;
    /// Write, even after a panic of another writer
    fn write_locked(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> RwLockExt<T> for RwLock<T> {
    fn read_locked(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_locked(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

pub trait CondvarExt {
    /// `Condvar::wait`, even after a panic of another holder
    fn wait_locked<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T>;
    /// `Condvar::wait_timeout`, even after a panic of another holder
    fn wait_timeout_locked<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
    ) -> MutexGuard<'a, T>;
}

impl CondvarExt for Condvar {
    fn wait_locked<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait(guard).unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_timeout_locked<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
    ) -> MutexGuard<'a, T> {
        self.wait_timeout(guard, timeout)
            .map_or_else(|e| e.into_inner().0, |(guard, _)| guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_lock_still_opens() {
        let shared = Arc::new(Mutex::new(1));
        let other = shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = other.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(shared.is_poisoned());
        *shared.locked() += 1;
        assert_eq!(*shared.locked(), 2);
    }
}
