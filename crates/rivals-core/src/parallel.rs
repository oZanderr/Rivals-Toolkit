//! Thread pools for parallel work that something else waits on.
//!
//! A rayon worker waiting for parallel work runs whatever else its pool has queued meanwhile, and
//! a thread outside a pool waits until one of the pool's workers takes its work. So a thread that
//! holds a lock while working in parallel can be handed work that waits on that lock, and work
//! that waits for a thread to drain a channel stalls when it fills the pool that thread needs.
//! Such work gets a pool of its own, entered from a thread of its own.

/// A pool of `threads` threads (at least one) for work no other pool's may share.
pub fn own_pool(threads: usize) -> Result<rayon::ThreadPool, String> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .build()
        .map_err(|e| format!("could not start {threads} threads: {e}"))
}

/// Runs `f` on a new thread, with its parallel work on a pool of its own as large as the one the
/// caller is on, and waits for it with a plain join. The caller runs nothing while it waits, so it
/// can hold a lock that other parallel work waits on, and `f` never waits on threads that do.
pub fn isolated<T: Send>(f: impl FnOnce() -> T + Send) -> Result<T, String> {
    let pool = own_pool(rayon::current_num_threads())?;
    let done = std::thread::scope(|scope| scope.spawn(|| pool.install(f)).join());
    Ok(done.unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// The only worker of a pool holds a lock while it waits on `isolated`, with work wanting the
    /// lock queued behind it. Waiting on another pool directly, it would run that work itself and
    /// wait on its own lock; waiting with a plain join, it gets back and lets go first.
    #[test]
    fn the_caller_runs_nothing_else_while_it_waits() {
        static LOCK: Mutex<()> = Mutex::new(());
        let pool = own_pool(1).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        pool.install(|| {
            let _held = LOCK.lock().unwrap();
            pool.spawn(move || {
                let _turn = LOCK.lock().unwrap();
                tx.send(()).unwrap();
            });
            isolated(|| std::thread::sleep(Duration::from_millis(50))).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the queued work ran once the lock was let go");
    }

    #[test]
    fn a_panic_inside_reaches_the_caller() {
        let caught = std::panic::catch_unwind(|| isolated(|| panic!("inside")));
        assert!(caught.is_err());
    }
}
