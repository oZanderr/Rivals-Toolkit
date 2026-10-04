// Hide the extra console window for Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// The default Windows heap makes threads queue for the large allocations a walk over the game
/// makes on every thread at once; mimalloc gives each thread its own.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    rivals_toolkit_lib::run()
}
