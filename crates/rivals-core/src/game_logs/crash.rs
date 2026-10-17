//! Reads a crash folder's CrashContext into the crash's type, message, call stack and loaded modules, telling the game's code from what else was loaded.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plugins::PROXY_DLL_NAMES;

pub const CONTEXT_FILE: &str = "CrashContext.runtime-xml";

/// .NET ticks (100 ns since 0001-01-01) at the Unix epoch.
const TICKS_AT_UNIX_EPOCH: i128 = 621_355_968_000_000_000;

/// User-mode graphics drivers, which load from the driver store rather than System32.
const GPU_DRIVER_PREFIXES: &[&str] = &[
    "nvwgf2um",
    "nvldumd",
    "nvd3dum",
    "nvgpucomp",
    "nvoglv",
    "amdxc",
    "amdxx",
    "atidxx",
    "atiu",
    "igd10",
    "igd12",
    "igdgmm",
    "igc64",
    "igdumd",
];

/// Windows modules a stack names without the module list's path.
const WINDOWS_MODULES: &[&str] = &[
    "ntdll",
    "kernel32",
    "kernelbase",
    "ucrtbase",
    "vcruntime140",
    "vcruntime140_1",
    "msvcp140",
    "user32",
    "gdi32",
    "win32u",
    "combase",
    "rpcrt4",
    "d3d12",
    "d3d12core",
    "dxgi",
    "d3d11",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleClass {
    /// Shipped in the game's install.
    Game,
    Windows,
    GpuDriver,
    /// Anything else in the process: ASI plugins, proxy loader DLLs, overlays, injected tools.
    ThirdParty,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub module: String,
    pub offset: Option<u64>,
    /// `Function() [file:line]`, for modules the crash reporter had symbols for.
    pub symbol: Option<String>,
    pub class: ModuleClass,
}

impl Frame {
    pub fn label(&self) -> String {
        match (&self.symbol, self.offset) {
            (Some(symbol), _) => format!("{}!{symbol}", self.module),
            (None, Some(offset)) => format!("{}+0x{offset:x}", self.module),
            (None, None) => self.module.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedModule {
    pub name: String,
    pub path: String,
    pub class: ModuleClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashReport {
    pub dir: String,
    /// `UECC-Windows-<GUID>_0000`.
    pub folder: String,
    /// `Crash`, `Assert`, `Ensure`, `Stall`, `GPUCrash` or `Hang`.
    pub crash_type: String,
    pub error_message: String,
    pub process_id: Option<u32>,
    pub seconds_since_start: Option<u64>,
    /// Unix seconds.
    pub crashed_at: Option<u64>,
    pub engine_version: Option<String>,
    pub build_version: Option<String>,
    pub command_line: Option<String>,
    pub cpu: Option<String>,
    pub gpu: Option<String>,
    pub gpu_driver: Option<String>,
    pub rhi: Option<String>,
    pub os: Option<String>,
    pub memory_gb: Option<u32>,
    pub out_of_memory: bool,
    pub crashed_thread: Option<String>,
    pub frames: Vec<Frame>,
    /// The first frame in a known module: where the crash happened.
    pub faulting_frame: Option<usize>,
    /// The first frame in the game's or a third party's code, past Windows and the drivers: the
    /// code that called into what crashed.
    pub culprit_frame: Option<usize>,
    pub modules: Vec<LoadedModule>,
    /// The message with heap addresses blanked and the top frames, alike for the same crash.
    pub signature: String,
    /// The session log the crash reporter copied into the folder.
    pub log_file: Option<String>,
}

impl CrashReport {
    pub fn third_party_modules(&self) -> impl Iterator<Item = &LoadedModule> {
        self.modules
            .iter()
            .filter(|m| m.class == ModuleClass::ThirdParty)
    }
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&end| end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The raw text between the first `<tag>` and its `</tag>`. The crash context is flat,
/// attribute-free XML the engine writes, so no general parser is needed.
fn inner<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    Some(&xml[start..end])
}

fn leaf(xml: &str, tag: &str) -> Option<String> {
    let text = unescape(inner(xml, tag)?.trim());
    (!text.is_empty()).then_some(text)
}

fn file_stem_lower(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_lowercase()
}

fn classify(path: &str, root: Option<&str>) -> ModuleClass {
    let lower = path.replace('/', "\\").to_lowercase();
    let name = lower.rsplit('\\').next().unwrap_or(&lower);
    if name.ends_with(".asi") {
        return ModuleClass::ThirdParty;
    }
    if let Some(root) = root
        && lower.starts_with(root)
    {
        let beside_exe = lower
            .strip_suffix(name)
            .is_some_and(|dir| dir.ends_with("\\binaries\\win64\\"));
        return if beside_exe && PROXY_DLL_NAMES.contains(&name) {
            ModuleClass::ThirdParty
        } else {
            ModuleClass::Game
        };
    }
    if lower.contains("\\driverstore\\") || GPU_DRIVER_PREFIXES.iter().any(|p| name.starts_with(p))
    {
        return ModuleClass::GpuDriver;
    }
    let windows = std::env::var("SystemRoot")
        .map(|root| format!("{}\\", root.to_lowercase()))
        .unwrap_or_else(|_| "c:\\windows\\".to_string());
    if lower.starts_with(&windows) {
        ModuleClass::Windows
    } else {
        ModuleClass::ThirdParty
    }
}

/// `Module 0xBASE + offset` triples, as `PCallStack` lists them.
fn portable_frames(text: &str) -> Vec<(String, Option<u64>)> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at + 2 < tokens.len() {
        if tokens[at + 2] == "+" && tokens[at + 1].starts_with("0x") {
            let offset = tokens
                .get(at + 3)
                .and_then(|hex| u64::from_str_radix(hex, 16).ok());
            out.push((tokens[at].to_string(), offset));
            at += 4;
        } else {
            at += 1;
        }
    }
    out
}

/// Hex numbers past the first 64 KB are heap and stack addresses, different every run; small
/// ones are offsets into a null object and say which field was read.
fn blank_addresses(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(at) = rest.find("0x") {
        out.push_str(&rest[..at]);
        let digits: String = rest[at + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        match u128::from_str_radix(&digits, 16) {
            Ok(value) if value > 0xFFFF => out.push_str("0x?"),
            _ => out.push_str(&rest[at..at + 2 + digits.len()]),
        }
        rest = &rest[at + 2 + digits.len()..];
    }
    out.push_str(rest);
    out
}

pub fn parse(xml: &str, dir: &Path) -> Result<CrashReport, String> {
    let runtime = inner(xml, "RuntimeProperties")
        .ok_or_else(|| format!("{} has no RuntimeProperties", dir.display()))?;
    // Each thread carries its own CallStack, which would shadow the crash's.
    let (props, threads) = match (runtime.find("<Threads>"), runtime.find("</Threads>")) {
        (Some(start), Some(end)) if start < end => (
            format!("{}{}", &runtime[..start], &runtime[end..]),
            &runtime[start..end],
        ),
        _ => (runtime.to_string(), ""),
    };
    let engine = inner(xml, "EngineData").unwrap_or("");
    let root = leaf(&props, "RootDir").map(|r| {
        let mut root = r.replace('/', "\\").to_lowercase();
        if !root.ends_with('\\') {
            root.push('\\');
        }
        root
    });

    let modules: Vec<LoadedModule> = leaf(&props, "Modules")
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| LoadedModule {
            name: path.rsplit(['\\', '/']).next().unwrap_or(path).to_string(),
            path: path.to_string(),
            class: classify(path, root.as_deref()),
        })
        .collect();
    let class_of = |module: &str| {
        let stem = module.to_lowercase();
        if stem == "unknown" {
            return ModuleClass::Unknown;
        }
        modules
            .iter()
            .find(|m| file_stem_lower(&m.name) == stem)
            .map(|m| m.class)
            // Code the loader never listed was mapped into the process by hand: injected.
            .unwrap_or(if WINDOWS_MODULES.contains(&stem.as_str()) {
                ModuleClass::Windows
            } else {
                ModuleClass::ThirdParty
            })
    };

    let portable = portable_frames(&leaf(&props, "PCallStack").unwrap_or_default());
    let symbolized: Vec<String> = leaf(&props, "CallStack")
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // The symbolized stack leaves out frames it could not name at the top, so the two line up
    // from the bottom.
    let skip = portable.len().saturating_sub(symbolized.len());
    let frames: Vec<Frame> = portable
        .into_iter()
        .enumerate()
        .map(|(at, (module, offset))| {
            let symbol = at
                .checked_sub(skip)
                .and_then(|i| symbolized.get(i))
                .and_then(|line| line.split_once('!'))
                .map(|(_, symbol)| symbol.to_string());
            Frame {
                class: class_of(&module),
                module,
                offset,
                symbol,
            }
        })
        .collect();
    let faulting_frame = frames.iter().position(|f| f.class != ModuleClass::Unknown);
    let culprit_frame = frames
        .iter()
        .position(|f| matches!(f.class, ModuleClass::Game | ModuleClass::ThirdParty));

    let crashed_thread = threads
        .split("<Thread>")
        .find(|thread| inner(thread, "IsCrashed").is_some_and(|v| v.trim() == "true"))
        .and_then(|thread| leaf(thread, "ThreadName"));

    let crash_type = leaf(&props, "CrashType").unwrap_or_else(|| "Crash".to_string());
    let error_message = leaf(&props, "ErrorMessage").unwrap_or_default();
    let top: Vec<String> = frames
        .iter()
        .filter(|f| f.class != ModuleClass::Unknown)
        .take(3)
        .map(|f| match f.offset {
            Some(offset) => format!("{}+0x{offset:x}", f.module.to_lowercase()),
            None => f.module.to_lowercase(),
        })
        .collect();
    let signature = format!(
        "{crash_type}|{}|{}",
        blank_addresses(&error_message),
        top.join(">")
    );

    let crashed_at = leaf(&props, "TimeOfCrash")
        .and_then(|t| t.parse::<i128>().ok())
        .and_then(|ticks| u64::try_from((ticks - TICKS_AT_UNIX_EPOCH) / 10_000_000).ok());
    let log_file = std::fs::read_dir(dir).ok().and_then(|entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("log")))
            .map(|p| p.to_string_lossy().into_owned())
    });

    Ok(CrashReport {
        dir: dir.to_string_lossy().into_owned(),
        folder: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        crash_type,
        error_message,
        process_id: leaf(&props, "ProcessId").and_then(|p| p.parse().ok()),
        seconds_since_start: leaf(&props, "SecondsSinceStart").and_then(|s| s.parse().ok()),
        crashed_at,
        engine_version: leaf(&props, "EngineVersion"),
        build_version: leaf(&props, "BuildVersion"),
        command_line: leaf(&props, "CommandLine"),
        cpu: leaf(&props, "Misc.CPUBrand").map(|c| c.trim().to_string()),
        gpu: leaf(engine, "RHI.AdapterName").or_else(|| leaf(&props, "Misc.PrimaryGPUBrand")),
        gpu_driver: leaf(engine, "RHI.UserDriverVersion"),
        rhi: leaf(engine, "RHI.RHIName"),
        os: leaf(&props, "Misc.OSVersionMajor"),
        memory_gb: leaf(&props, "MemoryStats.TotalPhysicalGB").and_then(|m| m.parse().ok()),
        out_of_memory: leaf(&props, "MemoryStats.bIsOOM").is_some_and(|m| m == "1"),
        crashed_thread,
        frames,
        faulting_frame,
        culprit_frame,
        modules,
        signature,
        log_file,
    })
}

pub fn read(dir: &Path) -> Result<CrashReport, String> {
    let path = dir.join(CONTEXT_FILE);
    let xml = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&xml, dir)
}

/// The crash folders under `crashes`, those holding a crash context.
pub fn crash_dirs(crashes: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(crashes) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join(CONTEXT_FILE).is_file())
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const CONTEXT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<FGenericCrashContext>
	<RuntimeProperties>
		<CrashType>Crash</CrashType>
		<ErrorMessage>Unhandled Exception: EXCEPTION_ACCESS_VIOLATION reading address 0x0000013ccf84c000</ErrorMessage>
		<ProcessId>14632</ProcessId>
		<SecondsSinceStart>87</SecondsSinceStart>
		<EngineVersion>5.3.2-3922109+++depot_marvel+S10.5_release</EngineVersion>
		<CommandLine>Marvel -bylauncher</CommandLine>
		<BuildVersion>3922109</BuildVersion>
		<RootDir>C:/Games/MarvelRivals/MarvelGame/</RootDir>
		<EpicAccountId>00000000000000000000000000000000</EpicAccountId>
		<Misc.CPUBrand>Some CPU   </Misc.CPUBrand>
		<MemoryStats.TotalPhysicalGB>32</MemoryStats.TotalPhysicalGB>
		<MemoryStats.bIsOOM>0</MemoryStats.bIsOOM>
		<CallStack>Tool!`anonymous namespace&apos;::Hook() [D:\src\Hook.cpp:613]
Marvel_Win64_Shipping
kernel32
ntdll</CallStack>
		<PCallStack>Unknown 0x0000000000000000 + 0 Tool 0x00007ffc4d450000 + 16dc31 Marvel-Win64-Shipping 0x00007ff6dd800000 + 245e66f KERNEL32 0x00007ffcdc920000 + 2cd87 ntdll 0x00007ffcdcb00000 + acaec</PCallStack>
		<Threads>
			<Thread>
				<CallStack>ntdll 0x00007ffcdcb00000 + 160e44</CallStack>
				<IsCrashed>false</IsCrashed>
				<ThreadName>GameThread</ThreadName>
			</Thread>
			<Thread>
				<CallStack />
				<IsCrashed>true</IsCrashed>
				<ThreadName>RenderThread 1</ThreadName>
			</Thread>
		</Threads>
		<TimeOfCrash>639272132047360000</TimeOfCrash>
		<Modules>C:\Games\MarvelRivals\MarvelGame\Marvel\Binaries\Win64\Marvel-Win64-Shipping.exe
C:\WINDOWS\System32\KERNEL32.DLL
C:\WINDOWS\SYSTEM32\ntdll.dll
D:\Tools\Tool.dll
C:\Games\MarvelRivals\MarvelGame\Marvel\Binaries\Win64\dsound.dll
C:\Games\MarvelRivals\MarvelGame\Marvel\Binaries\Win64\plugins\Bypass.asi
C:\WINDOWS\System32\DriverStore\FileRepository\nv_dispi.inf_amd64\nvwgf2umx.dll
C:\Games\MarvelRivals\MarvelGame\Engine\Binaries\ThirdParty\Ogg\Win64\VS2015\libogg_64.dll</Modules>
	</RuntimeProperties>
	<EngineData>
		<RHI.RHIName>D3D12</RHI.RHIName>
		<RHI.AdapterName>Some GPU</RHI.AdapterName>
		<RHI.UserDriverVersion>616.64</RHI.UserDriverVersion>
	</EngineData>
</FGenericCrashContext>"#;

    #[test]
    fn reads_the_crash_and_lines_the_stacks_up() {
        let crash = parse(CONTEXT, Path::new("UECC-Windows-ABC_0000")).unwrap();
        assert_eq!(crash.crash_type, "Crash");
        assert_eq!(crash.process_id, Some(14632));
        assert_eq!(crash.crashed_thread.as_deref(), Some("RenderThread 1"));
        assert_eq!(crash.cpu.as_deref(), Some("Some CPU"));
        assert_eq!(crash.gpu_driver.as_deref(), Some("616.64"));
        assert_eq!(crash.crashed_at, Some(1_791_616_404));
        assert_eq!(crash.frames.len(), 5);
        assert_eq!(crash.frames[0].class, ModuleClass::Unknown);
        assert_eq!(crash.frames[1].module, "Tool");
        assert_eq!(crash.frames[1].offset, Some(0x16dc31));
        assert_eq!(
            crash.frames[1].symbol.as_deref(),
            Some("`anonymous namespace'::Hook() [D:\\src\\Hook.cpp:613]")
        );
        assert_eq!(crash.frames[2].symbol, None);
        assert_eq!(crash.faulting_frame, Some(1));
        assert_eq!(crash.culprit_frame, Some(1));
        assert_eq!(crash.frames[1].class, ModuleClass::ThirdParty);
        assert_eq!(crash.frames[2].class, ModuleClass::Game);
        assert_eq!(crash.frames[3].class, ModuleClass::Windows);
    }

    #[test]
    fn tells_the_game_from_what_else_was_loaded() {
        let crash = parse(CONTEXT, Path::new("x")).unwrap();
        let class = |name: &str| {
            crash
                .modules
                .iter()
                .find(|m| m.name == name)
                .map(|m| m.class)
                .unwrap()
        };
        assert_eq!(class("Marvel-Win64-Shipping.exe"), ModuleClass::Game);
        assert_eq!(class("libogg_64.dll"), ModuleClass::Game);
        assert_eq!(class("dsound.dll"), ModuleClass::ThirdParty);
        assert_eq!(class("Bypass.asi"), ModuleClass::ThirdParty);
        assert_eq!(class("Tool.dll"), ModuleClass::ThirdParty);
        assert_eq!(class("nvwgf2umx.dll"), ModuleClass::GpuDriver);
        let third: Vec<&str> = crash
            .third_party_modules()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(third, ["Tool.dll", "dsound.dll", "Bypass.asi"]);
    }

    #[test]
    fn the_same_crash_signs_alike_whatever_the_heap_address() {
        let one = parse(CONTEXT, Path::new("a")).unwrap();
        let other = CONTEXT.replace("0x0000013ccf84c000", "0x0000023499e7c000");
        let two = parse(&other, Path::new("b")).unwrap();
        assert_eq!(one.signature, two.signature);
        let near_null = CONTEXT.replace("0x0000013ccf84c000", "0x0000000000000028");
        let three = parse(&near_null, Path::new("c")).unwrap();
        assert_ne!(one.signature, three.signature);
        assert!(three.signature.contains("0x0000000000000028"));
    }

    #[test]
    fn unescapes_entities() {
        assert_eq!(
            unescape("a &lt;b&gt; &amp;&quot;&apos; &#65;&#x42; & c"),
            "a <b> &\"' AB & c"
        );
    }
}
