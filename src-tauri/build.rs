fn main() {
    tauri_build::build();

    // Copy Oodle next to the binary so it's available in dev builds.
    let lib_name = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => "liboo2corelinux64.so.9",
        _ => "oo2core_9_win64.dll",
    };
    let lib_src = std::path::Path::new("resources").join(lib_name);
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bin_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR depth");
    let lib_dst = bin_dir.join(lib_name);
    if lib_src.exists() && !lib_dst.exists() {
        std::fs::copy(&lib_src, &lib_dst)
            .unwrap_or_else(|e| panic!("failed to copy {lib_name}: {e}"));
    }

    println!("cargo:rerun-if-changed=resources/{lib_name}");
}
