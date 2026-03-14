// Hide the extra console window for Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    oinkers_editor_lib::run()
}
