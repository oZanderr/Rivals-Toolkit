use std::sync::LazyLock;

/// Shared rayon pool sized to half the available cores so parallel work does
/// not starve the Tauri runtime threads. Build is fatal: a missing pool means
/// no parallel scans can run, so panic at first access rather than thread an
/// error through every call site.
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
