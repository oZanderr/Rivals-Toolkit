pub(crate) mod crypto;
mod reader;
mod writer;

pub(crate) fn list_pak_files(game_root: &str) -> Result<Vec<String>, String> {
    reader::list_pak_files(game_root)
}

pub(crate) fn list_pak_contents(pak_path: &str) -> Result<Vec<String>, String> {
    reader::list_pak_contents(pak_path)
}

pub(crate) fn unpack_pak(pak_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
    reader::unpack_pak(pak_path, output_dir)
}

pub(crate) fn extract_single_file(
    pak_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    reader::extract_single_file(pak_path, file_name, output_path)
}

pub(crate) fn repack_pak(input_dir: &str, output_pak: &str) -> Result<(), String> {
    writer::repack_pak(input_dir, output_pak)
}
