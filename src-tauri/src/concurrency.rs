//! Rayon thread pool sized to half the available cores so parallel scans and downstream library work (e.g. retoc's per-block Oodle compression) don't starve the Tauri runtime threads.

use std::sync::LazyLock;

fn thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(2)
}

/// Initialise rayon's global pool to a polite thread count. Called once at app
/// startup. Anything that calls `par_iter` without an explicit pool (our code,
/// retoc, repak) inherits this same budget. Idempotent: a failure here means
/// some other code already built the global pool; we let that stand.
pub(crate) fn init_global_pool() {
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count())
        .build_global();
}

#[allow(clippy::expect_used)]
pub(crate) static POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count())
        .build()
        .expect("failed to build scoped rayon pool")
});
