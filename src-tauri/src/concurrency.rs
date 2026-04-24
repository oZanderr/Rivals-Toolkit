//! Shared rayon thread pool sized to half the available cores so parallel scans don't starve the Tauri runtime threads.

use std::sync::LazyLock;

#[allow(clippy::expect_used)]
pub(crate) static POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(2);
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("failed to build scoped rayon pool")
});
