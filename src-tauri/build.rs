fn main() {
    tauri_build::build();

    // Copy oo2core_9_win64.dll next to the binary so Oodle is available in dev builds.
    let dll_src = std::path::Path::new("resources/oo2core_9_win64.dll");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bin_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR depth");
    let dll_dst = bin_dir.join("oo2core_9_win64.dll");
    if dll_src.exists() && !dll_dst.exists() {
        std::fs::copy(dll_src, &dll_dst)
            .unwrap_or_else(|e| panic!("failed to copy oo2core_9_win64.dll: {e}"));
    }

    println!("cargo:rerun-if-changed=resources/oo2core_9_win64.dll");
}
