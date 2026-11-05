//! Puts Oodle next to the CLI binary so pak reads work without the desktop app ever being built.

fn main() {
    let name = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => "liboo2corelinux64.so.9",
        _ => "oo2core_9_win64.dll",
    };
    // The library ships as a Tauri app resource; the CLI reaches for the same copy rather than
    // duplicating the blob in the repo.
    let src = std::path::Path::new("../../src-tauri/resources").join(name);
    println!("cargo:rerun-if-changed={}", src.display());

    let Some(bin_dir) = std::env::var("OUT_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(std::path::Path::to_path_buf))
    else {
        return;
    };

    let dst = bin_dir.join(name);
    if src.exists()
        && !dst.exists()
        && let Err(e) = std::fs::copy(&src, &dst)
    {
        // A missing library only degrades compressed-pak reads, so warn rather than fail the build.
        println!("cargo:warning=could not stage {name}: {e}");
    }
}
