//! Puts Oodle next to the CLI binary so pak reads work without the desktop app ever being built.

fn main() {
    // The DLL ships as a Tauri app resource; the CLI reaches for the same copy rather than
    // duplicating a 600 KB blob in the repo.
    let src = std::path::Path::new("../../src-tauri/resources/oo2core_9_win64.dll");
    println!("cargo:rerun-if-changed={}", src.display());

    let Some(bin_dir) = std::env::var("OUT_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(std::path::Path::to_path_buf))
    else {
        return;
    };

    let dst = bin_dir.join("oo2core_9_win64.dll");
    if src.exists()
        && !dst.exists()
        && let Err(e) = std::fs::copy(src, &dst)
    {
        // A missing DLL only degrades compressed-pak reads, so warn rather than fail the build.
        println!("cargo:warning=could not stage oo2core_9_win64.dll: {e}");
    }
}
