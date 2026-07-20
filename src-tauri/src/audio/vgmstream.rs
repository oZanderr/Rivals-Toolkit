//! Wraps the user-supplied vgmstream-cli decoder to convert Wwise WEM (any codec) to WAV bytes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::pcm::{ConvertError, Result};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Resolve the vgmstream-cli binary. Prefers the configured Settings path, then the
/// `RIVALS_VGMSTREAM` env override (used by the integration test). Returns `None` when
/// neither points at an existing file.
pub(crate) fn resolve(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = configured {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(p) = std::env::var_os("RIVALS_VGMSTREAM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Run the binary with no input and confirm it identifies itself as vgmstream.
/// Returns the version/identity banner line for display.
pub(crate) fn probe_version(bin: &Path) -> Result<String> {
    let mut cmd = Command::new(bin);
    no_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| ConvertError::Vgmstream(format!("could not launch that binary: {e}")))?;

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));

    match text
        .lines()
        .find(|l| l.to_ascii_lowercase().contains("vgmstream"))
    {
        Some(line) => Ok(line.trim().to_string()),
        None => Err(ConvertError::Vgmstream(
            "that binary does not look like vgmstream-cli".into(),
        )),
    }
}

/// Decode in-memory WEM bytes to WAV bytes via a managed temp round-trip.
pub(crate) fn decode_bytes_to_wav(bin: &Path, wem: &[u8]) -> Result<Vec<u8>> {
    let dir = scratch_dir()?;
    let stem = unique_stem();
    let input = TempFile(dir.join(format!("{stem}.wem")));
    let output = TempFile(dir.join(format!("{stem}.wav")));
    std::fs::write(&input.0, wem)?;
    run_decode(bin, &output.0, &input.0, None)?;
    read_output(&output.0)
}

/// Decode an on-disk audio file (WEM/BNK subsong/etc.) to WAV bytes.
pub(crate) fn decode_file_to_wav(
    bin: &Path,
    input: &Path,
    subsong: Option<u32>,
) -> Result<Vec<u8>> {
    let dir = scratch_dir()?;
    let output = TempFile(dir.join(format!("{}.wav", unique_stem())));
    run_decode(bin, &output.0, input, subsong)?;
    read_output(&output.0)
}

fn read_output(output: &Path) -> Result<Vec<u8>> {
    let wav = std::fs::read(output)?;
    if wav.is_empty() {
        return Err(ConvertError::Vgmstream(
            "vgmstream produced no output".into(),
        ));
    }
    Ok(wav)
}

fn scratch_dir() -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .ok_or_else(|| ConvertError::Vgmstream("cache directory unavailable".into()))?
        .join("rivals-toolkit")
        .join("vgmstream-tmp");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn unique_stem() -> String {
    format!(
        "{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// `vgmstream-cli -i -o <output> [-s <subsong>] <input>`. `-o` pins the output path so
/// multi-subsong inputs never emit stray `name_00n` files; `-i` ignores loop logic for a
/// single linear decode.
fn build_args(output: &Path, input: &Path, subsong: Option<u32>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("-i"),
        OsString::from("-o"),
        output.as_os_str().to_os_string(),
    ];
    if let Some(n) = subsong {
        args.push(OsString::from("-s"));
        args.push(OsString::from(n.to_string()));
    }
    args.push(input.as_os_str().to_os_string());
    args
}

fn run_decode(bin: &Path, output: &Path, input: &Path, subsong: Option<u32>) -> Result<()> {
    let mut cmd = Command::new(bin);
    cmd.args(build_args(output, input, subsong));
    no_window(&mut cmd);

    let out = cmd
        .output()
        .map_err(|e| ConvertError::Vgmstream(format!("failed to launch vgmstream-cli: {e}")))?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(ConvertError::Vgmstream(format!(
            "vgmstream-cli failed (code {code}): {}",
            msg.trim()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

/// Deletes the wrapped path on drop (best effort), so every error path cleans its temps.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg_strings(output: &str, input: &str, subsong: Option<u32>) -> Vec<String> {
        build_args(Path::new(output), Path::new(input), subsong)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn args_without_subsong() {
        assert_eq!(
            arg_strings("out.wav", "in.wem", None),
            ["-i", "-o", "out.wav", "in.wem"]
        );
    }

    #[test]
    fn args_with_subsong() {
        assert_eq!(
            arg_strings("out.wav", "in.bnk", Some(3)),
            ["-i", "-o", "out.wav", "-s", "3", "in.bnk"]
        );
    }

    #[test]
    fn args_preserve_paths_with_spaces() {
        let args = build_args(
            Path::new("C:/a b/out.wav"),
            Path::new("C:/c d/in.wem"),
            None,
        );
        // Paths stay single argv elements; no shell, so no quoting needed.
        assert_eq!(args[2].to_string_lossy(), "C:/a b/out.wav");
        assert_eq!(args[3].to_string_lossy(), "C:/c d/in.wem");
    }

    #[test]
    fn unique_stems_differ() {
        assert_ne!(unique_stem(), unique_stem());
    }

    #[test]
    fn resolve_returns_none_for_missing_path() {
        assert!(resolve(Some("definitely/not/a/real/binary")).is_none());
    }

    /// Gated on a real binary + fixture; CI without them passes by early return.
    #[test]
    fn decodes_known_wem() {
        let Some(_) = std::env::var_os("RIVALS_VGMSTREAM") else {
            return;
        };
        let Some(fixture) = std::env::var_os("VGMSTREAM_TEST_WEM") else {
            return;
        };
        let Some(bin) = resolve(None) else {
            panic!("RIVALS_VGMSTREAM should resolve to a file");
        };
        let wav = match decode_file_to_wav(&bin, Path::new(&fixture), None) {
            Ok(w) => w,
            Err(e) => panic!("decode of fixture WEM failed: {e}"),
        };
        assert!(wav.len() > 44, "WAV should have a header plus data");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }
}
