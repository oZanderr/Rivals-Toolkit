//! A cooked texture's platform data, read from the bytes a texture writes after its properties:
//! the pixel format, the size, and each mip with where its pixels are.
//!
//! The layout, after the property block and its guid word, as the game's textures write it:
//!
//! - two strip-flag words, the texture's and its class's, and the cooked flag;
//! - for a 2D texture, a word every one the game ships holds 1 in;
//! - per platform format, until a `None` name: the format's name, a skip offset counted from
//!   where it sits to the next format's name, 16 bytes a 2D texture leaves zero, then the size, the slice count and
//!   flags, the format as a string, the first mip to load, the mip count, and per mip the bulk
//!   data resource holding its pixels, those pixels when they sit inline, and its size; last, the
//!   virtual texture flag.

use retoc::legacy_asset::{FLegacyPackageHeader, FMinimalName, FObjectDataResource};
use serde::{Deserialize, Serialize};

use crate::mappings::Mappings;
use crate::package::{
    AssetBundle, ExportStatus, ParseOptions, ParsedExport, ParsedPackage, read_header,
};
use crate::reader::Cursor;
use crate::write::{
    IN_SEPARATE_FILE, OPTIONAL_PAYLOAD, SEPARATE_PAYLOAD_FLAGS, bulk_lock, placement,
};

/// `PackedData`'s top bit: the texture is a cubemap.
const CUBEMAP: u32 = 1 << 31;
/// `PackedData`'s next bit: the platform data carries its optional words.
const HAS_OPT_DATA: u32 = 1 << 30;
const SLICES: u32 = HAS_OPT_DATA - 1;

/// Why a virtual texture's platform data is not read: its tiles follow in a layout of their own.
pub(crate) const VIRTUAL: &str = "the texture is virtual, whose tile data this does not read; \
                                  the game ships only the engine's placeholder ones";

#[derive(Debug, Clone, Serialize)]
pub struct TextureMip {
    pub size_x: u32,
    pub size_y: u32,
    pub size_z: u32,
    /// The bulk data resource holding its pixels.
    pub resource: i32,
    /// Where they are: `inline`, `ubulk`, `uptnl` or `m.ubulk`.
    pub placement: &'static str,
    /// How many bytes of pixels it has.
    pub size: i64,
    /// For pixels inline, where they start in the export data.
    #[serde(skip)]
    pub inline_at: Option<usize>,
    /// Where the bulk data table puts them: from the export's start when inline, else from the
    /// start of the file they are in.
    #[serde(skip)]
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TexturePlatform {
    /// The pixel format, as `PF_B8G8R8A8`.
    pub format: String,
    pub size_x: u32,
    pub size_y: u32,
    pub slices: u32,
    pub cubemap: bool,
    /// The optional words: extra data and how many mips the tail packs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opt_data: Option<(u32, u32)>,
    pub first_mip: i32,
    pub mips: Vec<TextureMip>,
    pub is_virtual: bool,
    /// The format's name as the package stores it, for writing it back.
    #[serde(skip)]
    pub format_name: FMinimalName,
    /// The 16 bytes before the size: zero in a 2D texture, kept as they were in the others.
    #[serde(skip)]
    pub reserved: [u8; 16],
    /// Where the format's name starts and where the next one does, in the export data.
    #[serde(skip)]
    pub span: (usize, usize),
}

#[derive(Debug, Clone, Serialize)]
pub struct TextureLayout {
    pub export: u32,
    pub class: String,
    pub cooked: bool,
    /// The word a 2D texture writes after the cooked flag, 1 in every one the game ships. Other
    /// texture classes go straight on to their formats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mip_data_word: Option<u32>,
    pub platforms: Vec<TexturePlatform>,
    /// The strip-flag words, kept as they were.
    #[serde(skip)]
    pub strip_flags: [u8; 4],
    /// The `None` that ends the list of formats, as the package stores it.
    #[serde(skip)]
    pub end_name: FMinimalName,
    /// Where the platform data starts, after the guid word, and where the export ends, in the
    /// export data.
    #[serde(skip)]
    pub span: (usize, usize),
}

impl TextureLayout {
    /// The platform data the game reads: the only format cooked, as every texture here has.
    pub fn platform(&self) -> Result<&TexturePlatform, String> {
        match self.platforms.as_slice() {
            [one] => Ok(one),
            [] => Err("the texture has no cooked platform data".into()),
            _ => Err(format!(
                "the texture is cooked for {} pixel formats, where one is all this reads",
                self.platforms.len()
            )),
        }
    }
}

/// Whether `class` is one whose exports carry texture platform data.
pub fn is_texture_class(class: &str) -> bool {
    matches!(
        class,
        "Texture2D" | "TextureCube" | "Texture2DArray" | "VolumeTexture" | "TextureCubeArray"
    )
}

/// The platform data of texture `export`, read from the bytes after its properties.
pub fn read_texture(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    export: u32,
) -> Result<TextureLayout, String> {
    let parsed_export = parsed
        .exports
        .get(export as usize)
        .ok_or_else(|| format!("the package has no export {export}"))?;
    read_texture_export(bundle, parsed_export)
}

/// [`read_texture`] from the texture's export as parsed, alone or with the rest of its package.
pub fn read_texture_export(
    bundle: &AssetBundle<'_>,
    parsed_export: &crate::package::ParsedExport,
) -> Result<TextureLayout, String> {
    let header = read_header(bundle)?;
    let export = parsed_export.index;
    let properties_end = parsed_export.properties_end.ok_or_else(|| {
        format!(
            "{}'s properties did not read whole, so where its platform data starts is unknown",
            parsed_export.object_name
        )
    })?;
    read_texture_at(
        &header,
        bundle.exports,
        export as usize,
        properties_end,
        &parsed_export.class_name,
    )
}

fn read_texture_at(
    header: &FLegacyPackageHeader,
    exports: &[u8],
    export: usize,
    properties_end: u64,
    class: &str,
) -> Result<TextureLayout, String> {
    let total = i64::from(header.summary.versioning_info.total_header_size);
    let entry = header
        .exports
        .get(export)
        .ok_or_else(|| format!("the package has no export {export}"))?;
    let first = usize::try_from(entry.serial_offset - total)
        .map_err(|_| "the export starts before its data".to_string())?;
    let end = first
        + usize::try_from(entry.serial_size).map_err(|_| "the export has no size".to_string())?;
    let start = usize::try_from(properties_end as i64 - total)
        .map_err(|_| "the properties end before the export data".to_string())?;
    if start < first || start > end || end > exports.len() {
        return Err("the texture's properties end outside its export".into());
    }
    let mut cursor = Cursor::new(&exports[start..end], start as u64 + total as u64);
    match cursor.read_u32()? {
        0 => {}
        1 => cursor.skip(16)?,
        other => return Err(cursor.err(format!("guid flag {other} is neither 0 nor 1"))),
    }
    let mut layout = read_platform_data(&mut cursor, header, class, entry.serial_offset)?;
    layout.export = export as u32;
    Ok(layout)
}

/// The platform data a texture of `class` writes after its object guid, read from `cursor` to the
/// end of the export, every name in it on the cursor's record. `export_start` is where the export
/// starts in the file, which is what an inline mip's place in the bulk data table counts from. The
/// layout's export is left for the caller to say.
pub(crate) fn read_platform_data(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    class: &str,
    export_start: i64,
) -> Result<TextureLayout, String> {
    let total = i64::from(header.summary.versioning_info.total_header_size);
    // Places in the export data, which is where the layout keeps them.
    let at = |cursor: &Cursor<'_>| (cursor.file_offset() as i64 - total) as usize;
    let span_start = at(cursor);
    let mut strip_flags = [0u8; 4];
    strip_flags.copy_from_slice(cursor.take(4)?);
    let cooked = cursor.read_bool32()?;
    if !cooked {
        return Err("the texture is not cooked, so it carries no platform data".into());
    }
    let mip_data_word = if class == "Texture2D" {
        Some(cursor.read_u32()?)
    } else {
        None
    };

    let mut platforms = Vec::new();
    let end_name = loop {
        let name_at = at(cursor);
        let (format_name, id) = cursor.read_name_id(&header.name_map)?;
        if format_name == "None" && id.number == 0 {
            break id;
        }
        let skip_at = at(cursor);
        let skip = cursor.read_i64()?;
        let next = usize::try_from(skip_at as i64 + skip)
            .map_err(|_| cursor.err("the skip offset points before the texture"))?;
        let mut reserved = [0u8; 16];
        reserved.copy_from_slice(cursor.take(16)?);
        let size_x = cursor.read_u32()?;
        let size_y = cursor.read_u32()?;
        let packed = cursor.read_u32()?;
        let format = cursor.read_string()?;
        let opt_data = if packed & HAS_OPT_DATA != 0 {
            Some((cursor.read_u32()?, cursor.read_u32()?))
        } else {
            None
        };
        let first_mip = cursor.read_i32()?;
        let count = cursor.read_i32()?;
        let count = usize::try_from(count)
            .ok()
            .filter(|count| *count <= 32)
            .ok_or_else(|| cursor.err(format!("{count} mips")))?;
        let mut mips = Vec::with_capacity(count);
        for _ in 0..count {
            let resource = cursor.read_i32()?;
            let data = usize::try_from(resource)
                .ok()
                .and_then(|index| header.data_resources.get(index))
                .ok_or_else(|| {
                    cursor.err(format!("mip resource {resource} is not in the table"))
                })?;
            let flags = data.legacy_bulk_data_flags;
            let inline_at = if flags & SEPARATE_PAYLOAD_FLAGS == 0 {
                let here = at(cursor);
                if cursor.file_offset() as i64 != export_start + data.serial_offset {
                    return Err(cursor.err(format!(
                        "mip resource {resource} is inline, but the table puts it elsewhere"
                    )));
                }
                let size = usize::try_from(data.serial_size)
                    .map_err(|_| cursor.err("an inline mip has no size"))?;
                cursor.skip(size)?;
                Some(here)
            } else {
                None
            };
            mips.push(TextureMip {
                resource,
                placement: placement(flags),
                size: data.serial_size,
                inline_at,
                offset: data.serial_offset,
                size_x: cursor.read_u32()?,
                size_y: cursor.read_u32()?,
                size_z: cursor.read_u32()?,
            });
        }
        let is_virtual = cursor.read_bool32()?;
        if is_virtual {
            return Err(VIRTUAL.into());
        }
        if at(cursor) != next {
            return Err(cursor.err(format!(
                "the platform data ends {} bytes from where its skip offset points",
                next as i64 - at(cursor) as i64
            )));
        }
        platforms.push(TexturePlatform {
            format: format.clone(),
            size_x,
            size_y,
            slices: packed & SLICES,
            cubemap: packed & CUBEMAP != 0,
            opt_data,
            first_mip,
            mips,
            is_virtual,
            format_name: id,
            reserved,
            span: (name_at, next),
        });
        if format_name != format {
            return Err(cursor.err(format!(
                "the format is named {format_name} but written as {format}"
            )));
        }
    };
    if cursor.remaining() != 0 {
        return Err(cursor.err(format!(
            "{} bytes follow the platform data",
            cursor.remaining()
        )));
    }
    Ok(TextureLayout {
        export: 0,
        class: class.to_string(),
        cooked,
        mip_data_word,
        platforms,
        strip_flags,
        end_name,
        span: (span_start, at(cursor)),
    })
}

fn push_name(out: &mut Vec<u8>, name: FMinimalName) {
    out.extend_from_slice(&name.index.to_le_bytes());
    out.extend_from_slice(&name.number.to_le_bytes());
}

/// The bytes `layout` reads from, from its strip flags to the end of its export, with `inline`
/// the pixels of each inline mip in the order the mips come. Each skip offset is worked out from
/// what it skips; an inline mip's place in the bulk data table is the caller's to keep.
pub fn encode_texture(layout: &TextureLayout, inline: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(&layout.strip_flags);
    out.extend_from_slice(&u32::from(layout.cooked).to_le_bytes());
    if let Some(word) = layout.mip_data_word {
        out.extend_from_slice(&word.to_le_bytes());
    }
    let mut pixels = inline.iter();
    for platform in &layout.platforms {
        push_name(&mut out, platform.format_name);
        let skip_at = out.len();
        out.extend_from_slice(&0i64.to_le_bytes());
        out.extend_from_slice(&platform.reserved);
        out.extend_from_slice(&platform.size_x.to_le_bytes());
        out.extend_from_slice(&platform.size_y.to_le_bytes());
        let packed = platform.slices
            | if platform.cubemap { CUBEMAP } else { 0 }
            | if platform.opt_data.is_some() {
                HAS_OPT_DATA
            } else {
                0
            };
        out.extend_from_slice(&packed.to_le_bytes());
        out.extend_from_slice(&crate::edit::encode_string(&platform.format));
        if let Some((extra, tail_mips)) = platform.opt_data {
            out.extend_from_slice(&extra.to_le_bytes());
            out.extend_from_slice(&tail_mips.to_le_bytes());
        }
        out.extend_from_slice(&platform.first_mip.to_le_bytes());
        out.extend_from_slice(&(platform.mips.len() as i32).to_le_bytes());
        for mip in &platform.mips {
            out.extend_from_slice(&mip.resource.to_le_bytes());
            if mip.placement == "inline" {
                let bytes = pixels
                    .next()
                    .ok_or("fewer inline mips given than the texture has")?;
                out.extend_from_slice(bytes);
            }
            out.extend_from_slice(&mip.size_x.to_le_bytes());
            out.extend_from_slice(&mip.size_y.to_le_bytes());
            out.extend_from_slice(&mip.size_z.to_le_bytes());
        }
        out.extend_from_slice(&u32::from(platform.is_virtual).to_le_bytes());
        let skip = (out.len() - skip_at) as i64;
        out[skip_at..skip_at + 8].copy_from_slice(&skip.to_le_bytes());
    }
    if pixels.next().is_some() {
        return Err("more inline mips given than the texture has".into());
    }
    push_name(&mut out, layout.end_name);
    Ok(out)
}

/// A mip's new pixels, already in the texture's own pixel format: its layers back to back, a cube's
/// six faces, an array's slices or a volume's depth, each `size_x` by `size_y`.
#[derive(Debug, Clone)]
pub struct NewMip {
    pub size_x: u32,
    pub size_y: u32,
    /// The depth the mip records: 1 for a 2D texture or a cube, the slice count for an array, the
    /// depth for a volume.
    pub size_z: u32,
    pub bytes: Vec<u8>,
}

/// New pixels for a texture: every mip, largest first. A save of its own, since the texture's
/// platform data and bulk data table are written again around them.
#[derive(Debug, Clone)]
pub struct TextureEdit {
    pub export: u32,
    pub mips: Vec<NewMip>,
}

/// An image to give a texture the pixels of, as an edit file names one. It is encoded in the
/// texture's own format against the package as the save reads it, which the caller does: see
/// `rivals_core::texture`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextureImage {
    #[serde(default)]
    pub export: u32,
    /// The texture by path rather than by index, as `asset info` prints it or below the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// The image: a PNG, or an OpenEXR (`.exr`) file for a texture holding high dynamic range
    /// values, a cube's faces and an array's or a volume's slices stacked top to bottom.
    pub image: String,
    /// How hard BC7 and BC6H encode: `fast`, `default` or `best`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
}

/// Why a texture's pixels cannot be replaced, when they cannot: a layout this does not rebuild.
pub fn texture_lock(layout: &TextureLayout) -> Option<String> {
    let platform = match layout.platform() {
        Ok(platform) => platform,
        Err(e) => return Some(e),
    };
    if platform.is_virtual {
        return Some(VIRTUAL.into());
    }
    // Only a 2D texture writes the word before its formats.
    let (word, layered) = match layout.class.as_str() {
        "Texture2D" => (Some(1), platform.slices == 1 && !platform.cubemap),
        "TextureCube" => (None, platform.slices == 6 && platform.cubemap),
        "Texture2DArray" | "VolumeTexture" => (None, platform.slices >= 1 && !platform.cubemap),
        other => {
            return Some(format!(
                "it is a {other}, whose layers this does not lay out; the game ships none"
            ));
        }
    };
    if layout.mip_data_word != word {
        return Some("its platform data starts with a word no texture the game ships has".into());
    }
    if !layered {
        return Some(format!(
            "it records {} layer(s){}, which a {} does not have",
            platform.slices,
            if platform.cubemap { " as a cube" } else { "" },
            layout.class
        ));
    }
    if platform.opt_data.is_some() {
        return Some(
            "its platform data carries optional words no texture the game ships has".into(),
        );
    }
    None
}

/// Why texture `layout`'s pixels cannot be saved into `bundle`, when they cannot: its own layout
/// (see [`texture_lock`]), or bulk data the save would have to write again and cannot. The save
/// rewrites the texture's own entries in the bulk data table and, when the texture keeps mips
/// there, the package's `.ubulk`, every other payload in it carried over whole. A dry run asks
/// this as the save does.
pub fn texture_save_lock(
    bundle: &AssetBundle<'_>,
    layout: &TextureLayout,
    sidecars: crate::edit::Sidecars<'_>,
) -> Option<String> {
    if let Some(reason) = texture_lock(layout) {
        return Some(reason);
    }
    let header = match read_header(bundle) {
        Ok(header) => header,
        Err(e) => return Some(e),
    };
    let platform = layout.platform().ok()?;
    own_resources(&header.data_resources, platform, sidecars).err()
}

/// The texture's own entries in `table`, in mip order, or why its bulk data cannot be written
/// again: an entry shared or out of range, a payload stored in a way this does not write, or a
/// `.ubulk` whose other payloads this cannot carry over.
fn own_resources(
    table: &[FObjectDataResource],
    platform: &TexturePlatform,
    sidecars: crate::edit::Sidecars<'_>,
) -> Result<Vec<usize>, String> {
    let mut own = Vec::with_capacity(platform.mips.len());
    for mip in &platform.mips {
        let index = usize::try_from(mip.resource)
            .ok()
            .filter(|&index| index < table.len())
            .ok_or_else(|| {
                format!(
                    "a mip's bulk data resource, {}, is not in the table",
                    mip.resource
                )
            })?;
        if own.contains(&index) {
            return Err(format!("two mips share bulk data resource {index}"));
        }
        if let Some(reason) = bulk_lock(&table[index]) {
            return Err(format!("bulk data resource {index}: {reason}"));
        }
        own.push(index);
    }
    if sidecars.optional_bulk.is_some() {
        return Err("the package carries a .uptnl, which this does not rebuild".into());
    }
    // The texture's mips that are not inline go to the `.ubulk`, which is then written again.
    if platform.mips.iter().any(|mip| mip.placement != "inline") {
        others_in_ubulk(table, &own, sidecars.bulk)?;
    }
    Ok(own)
}

/// The payloads in the package's `.ubulk` that are not the texture's, in the order they sit there,
/// once the file is known to be tiled by its entries from its start: what a rewrite of it carries
/// over whole.
fn others_in_ubulk(
    table: &[FObjectDataResource],
    own: &[usize],
    file: Option<&[u8]>,
) -> Result<Vec<usize>, String> {
    let mut members: Vec<usize> = (0..table.len())
        .filter(|&index| {
            table[index].legacy_bulk_data_flags & SEPARATE_PAYLOAD_FLAGS == IN_SEPARATE_FILE
        })
        .collect();
    if members.iter().all(|index| own.contains(index)) {
        return Ok(Vec::new());
    }
    let file = file.ok_or(
        "the package's .ubulk holds other payloads too, and it was not read with the package",
    )?;
    // An empty entry sorts before one starting where it does.
    members.sort_by_key(|&index| (table[index].serial_offset, table[index].serial_size));
    let mut running = 0i64;
    for &index in &members {
        let resource = &table[index];
        if resource.cooked_index.is_some_and(|cooked| cooked != 0) {
            return Err(format!(
                "bulk data resource {index} sits in a numbered bulk file this editor does not write"
            ));
        }
        if resource.serial_offset != running {
            return Err(format!(
                "bulk data resource {index} does not follow the one before it in the .ubulk, so \
                 the file cannot be written again safely"
            ));
        }
        running += resource.serial_size;
    }
    if running != file.len() as i64 {
        return Err(format!(
            "the .ubulk's entries cover {running} bytes but the file holds {}",
            file.len()
        ));
    }
    members.retain(|index| !own.contains(index));
    Ok(members)
}

impl TexturePlatform {
    /// The size the texture gives itself when its largest stored mip is `top`. A cook that left out
    /// the largest mips keeps the full size, twice the largest stored mip's for each it left out,
    /// so a mip the size it stores now keeps the size it has.
    pub fn full_size(&self, top: (u32, u32)) -> (u32, u32) {
        if self.mips.first().map(|mip| (mip.size_x, mip.size_y)) == Some(top) {
            return (self.size_x, self.size_y);
        }
        let scale = 1u32 << self.first_mip.clamp(0, 16);
        (top.0.saturating_mul(scale), top.1.saturating_mul(scale))
    }
}

/// The bytes `layout` reads from, with where each inline mip's pixels start in them.
fn encode_placed(
    layout: &TextureLayout,
    inline: &[&[u8]],
) -> Result<(Vec<u8>, Vec<usize>), String> {
    let bytes = encode_texture(layout, inline)?;
    // The encoder writes each inline payload right after its mip's resource word, so their places
    // follow from the sizes in front of them.
    let mut at = Vec::new();
    let mut position = 4 + 4 + layout.mip_data_word.map_or(0, |_| 4);
    for platform in &layout.platforms {
        position += 8 + 8 + 16 + 12 + crate::edit::encode_string(&platform.format).len();
        if platform.opt_data.is_some() {
            position += 8;
        }
        position += 8;
        for mip in &platform.mips {
            position += 4;
            if mip.placement == "inline" {
                at.push(position);
                position += mip.size as usize;
            }
            position += 12;
        }
        position += 4;
    }
    if position + 8 != bytes.len() {
        return Err("the encoded platform data is not the length its parts add up to".into());
    }
    Ok((bytes, at))
}

/// Writes `edit`'s mips over texture `edit.export`: its platform data takes the new size and mips,
/// its entries in the package's bulk data table and its payloads in the `.ubulk` are written again
/// around them, and its imported size follows. Other exports' bulk data keep their entries and
/// bytes, moved in the `.ubulk` as the texture's grow or shrink.
pub(crate) fn patch_texture(
    bundle: &AssetBundle<'_>,
    sidecars: crate::edit::Sidecars<'_>,
    texture: &ParsedExport,
    edit: &TextureEdit,
) -> Result<crate::edit::PatchedBundle, String> {
    let header = read_header(bundle)?;
    let layout = read_texture_export(bundle, texture)?;
    let platform = layout.platform()?;
    let locked = |reason: String| format!("the texture's pixels cannot be replaced: {reason}");
    if let Some(reason) = texture_lock(&layout) {
        return Err(locked(reason));
    }
    let own = own_resources(&header.data_resources, platform, sidecars).map_err(locked)?;
    let source = &platform.mips;

    let first = edit.mips.first().ok_or("no mips given")?;
    if edit.mips.len() > 32 {
        return Err("more than 32 mips".into());
    }
    // A cube keeps its six faces and an array its slices at every mip, where a volume's depth
    // halves with its width and height.
    let volume = layout.class == "VolumeTexture";
    let flat = matches!(layout.class.as_str(), "Texture2D" | "TextureCube");
    if flat && edit.mips.iter().any(|mip| mip.size_z != 1) {
        return Err(format!("a {}'s mips each have a depth of 1", layout.class));
    }
    for pair in edit.mips.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let z = if volume {
            (a.size_z / 2).max(1)
        } else {
            a.size_z
        };
        let wanted = ((a.size_x / 2).max(1), (a.size_y / 2).max(1), z);
        if (b.size_x, b.size_y, b.size_z) != wanted {
            return Err(format!(
                "a {}x{}x{} mip is followed by a {}x{}x{} one, not a {}x{}x{} one",
                a.size_x, a.size_y, a.size_z, b.size_x, b.size_y, b.size_z, wanted.0, wanted.1, z
            ));
        }
    }

    // The smallest mips stay inline as many as the texture kept there, and the rest go to the
    // `.ubulk`, which the game installs where it does not install the `.uptnl`.
    let resource_of = |mip: &TextureMip| header.data_resources[mip.resource as usize];
    let inline_template = source
        .iter()
        .find(|mip| mip.placement == "inline")
        .map(resource_of);
    let separate_template = source
        .iter()
        .find(|mip| mip.placement != "inline")
        .map(|mip| {
            let mut resource = resource_of(mip);
            if mip.placement == "uptnl" {
                resource.legacy_bulk_data_flags =
                    (resource.legacy_bulk_data_flags & !OPTIONAL_PAYLOAD) | IN_SEPARATE_FILE;
            }
            resource
        });
    let kept_inline = source
        .iter()
        .rev()
        .take_while(|mip| mip.placement == "inline")
        .count();
    let count = edit.mips.len();
    let inline_count = match &separate_template {
        Some(_) => kept_inline.min(count - 1),
        None => count,
    };
    // The new mips take the texture's own entries, lowest first. More mips than it had get entries
    // added at the end of the table; fewer leave the rest empty, or drop them where they end it.
    let mut table = header.data_resources.clone();
    let mut pool = own.clone();
    pool.sort_unstable();
    let mut slots = Vec::with_capacity(count);
    let mut separate: Vec<(usize, &[u8])> = Vec::new();
    for (index, mip) in edit.mips.iter().enumerate() {
        let inline = index >= count - inline_count;
        let mut resource = if inline {
            inline_template
        } else {
            separate_template
        }
        .ok_or("the texture has no mip of the kind this one needs to copy")?;
        let size = mip.bytes.len() as i64;
        resource.serial_offset = 0;
        resource.serial_size = size;
        resource.raw_size = size;
        let slot = match pool.get(index) {
            Some(&slot) => {
                table[slot] = resource;
                slot
            }
            None => {
                table.push(resource);
                table.len() - 1
            }
        };
        if !inline {
            separate.push((slot, &mip.bytes));
        }
        slots.push(slot);
    }

    // The `.ubulk` keeps every other payload in it, in its order, and takes the texture's after.
    let ubulk = if separate.is_empty() {
        None
    } else {
        let others = others_in_ubulk(&header.data_resources, &own, sidecars.bulk)?;
        let mut out = Vec::new();
        for &index in &others {
            let resource = &header.data_resources[index];
            let bytes = sidecars
                .bulk
                .and_then(|file| {
                    file.get(resource.serial_offset as usize..)?
                        .get(..resource.serial_size as usize)
                })
                .ok_or_else(|| format!("bulk data resource {index} runs past the .ubulk's end"))?;
            table[index].serial_offset = out.len() as i64;
            out.extend_from_slice(bytes);
        }
        for (slot, bytes) in &separate {
            table[*slot].serial_offset = out.len() as i64;
            out.extend_from_slice(bytes);
        }
        Some(out)
    };
    let ubulk_end = ubulk
        .as_ref()
        .map_or_else(|| sidecars.bulk.map_or(0, <[u8]>::len), Vec::len) as i64;
    let dropped = pool.get(count..).unwrap_or_default();
    for &slot in dropped {
        let template = header.data_resources[slot];
        table[slot] = FObjectDataResource {
            serial_offset: ubulk_end,
            serial_size: 0,
            raw_size: 0,
            legacy_bulk_data_flags: (template.legacy_bulk_data_flags & !SEPARATE_PAYLOAD_FLAGS)
                | IN_SEPARATE_FILE,
            ..template
        };
    }
    while table
        .len()
        .checked_sub(1)
        .is_some_and(|last| dropped.contains(&last))
    {
        table.pop();
    }
    let mips: Vec<TextureMip> = edit
        .mips
        .iter()
        .zip(&slots)
        .map(|(mip, &slot)| TextureMip {
            size_x: mip.size_x,
            size_y: mip.size_y,
            size_z: mip.size_z,
            resource: slot as i32,
            placement: placement(table[slot].legacy_bulk_data_flags),
            size: mip.bytes.len() as i64,
            inline_at: None,
            offset: table[slot].serial_offset,
        })
        .collect();
    // The texture's own size, which is the full one when the cook left out its largest mips: the
    // new mips start where the stored ones did, as many below it.
    let was = (platform.size_x, platform.size_y);
    let now = platform.full_size((first.size_x, first.size_y));
    let mut rebuilt = layout.clone();
    rebuilt.platforms[0].size_x = now.0;
    rebuilt.platforms[0].size_y = now.1;
    // An array's or a volume's slice count is its largest mip's depth.
    if !flat {
        rebuilt.platforms[0].slices = first.size_z;
    }
    rebuilt.platforms[0].mips = mips;
    let inline_bytes: Vec<&[u8]> = edit.mips[count - inline_count..]
        .iter()
        .map(|mip| mip.bytes.as_slice())
        .collect();
    let (tail, inline_at) = encode_placed(&rebuilt, &inline_bytes)?;

    let total = i64::from(header.summary.versioning_info.total_header_size);
    let entry = &header.exports[edit.export as usize];
    let first_byte = (entry.serial_offset - total) as usize;
    let end_byte = first_byte + entry.serial_size as usize;
    let mut export_bytes = bundle.exports[first_byte..layout.span.0].to_vec();
    let properties = export_bytes.len();
    for (k, at) in inline_at.iter().enumerate() {
        table[slots[count - inline_count + k]].serial_offset = (properties + at) as i64;
    }
    export_bytes.extend_from_slice(&tail);

    let mut notes = Vec::new();
    // Only a 2D texture records the size it was imported at.
    if was != now && layout.class == "Texture2D" {
        match texture
            .properties
            .iter()
            .find(|p| p.name == "ImportedSize")
            .and_then(|p| p.span)
        {
            Some((start, end)) if end - start == 8 => {
                let at = (start as i64 - total) as usize - first_byte;
                export_bytes[at..at + 4].copy_from_slice(&(now.0 as i32).to_le_bytes());
                export_bytes[at + 4..at + 8].copy_from_slice(&(now.1 as i32).to_le_bytes());
            }
            _ => notes.push(
                "the texture stores no imported size where it can be set, so it keeps none".into(),
            ),
        }
    }
    if source.iter().any(|mip| mip.placement == "uptnl") {
        notes.push(
            "the texture's largest mips were optional data the game does not install; the new ones \
             are in its .ubulk, so they load with it"
                .into(),
        );
    }

    let splice = crate::write::Splice {
        start: (total as usize + first_byte) as u64,
        end: (total as usize + end_byte) as u64,
        bytes: export_bytes,
    };
    let start = splice.start;
    let rewritten = crate::write::rewrite(
        bundle,
        &[splice],
        crate::write::HeaderDraft {
            data_resources: Some(table),
            ..Default::default()
        },
    )?;
    let after = AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    };
    crate::write::check_inline_bulk(&after)?;
    check_others(
        &header,
        bundle,
        sidecars.bulk,
        &after,
        ubulk.as_deref(),
        &own,
    )?;
    check_pixels(&after, ubulk.as_deref(), texture, total, edit)?;

    Ok(crate::edit::PatchedBundle {
        asset: rewritten.asset,
        exports: rewritten.exports,
        applied: vec![crate::edit::AppliedEdit {
            name: format!("{} pixels", texture.object_name),
            offset: start,
            offset_after: start,
            element: None,
            elements_after: None,
            before: format!("{}x{}, {} mip(s)", was.0, was.1, source.len()),
            after: format!(
                "{}x{}, {} mip(s), {} inline",
                now.0, now.1, count, inline_count
            ),
        }],
        bulk: ubulk,
        optional_bulk: None,
        notes,
    })
}

/// Holds every bulk data payload that is not the texture's to the bytes it had: inline ones where
/// their exports hold them, the `.ubulk`'s in the file written, and the rest to their entries.
fn check_others(
    header: &FLegacyPackageHeader,
    before: &AssetBundle<'_>,
    ubulk_before: Option<&[u8]>,
    after: &AssetBundle<'_>,
    ubulk_after: Option<&[u8]>,
    own: &[usize],
) -> Result<(), String> {
    use crate::write::locate_inline_payload;

    let rewritten = read_header(after)?;
    let payload = |file: Option<&[u8]>, resource: &FObjectDataResource| -> Option<Vec<u8>> {
        file?
            .get(resource.serial_offset as usize..)?
            .get(..resource.serial_size as usize)
            .map(<[u8]>::to_vec)
    };
    for (index, was) in header.data_resources.iter().enumerate() {
        if own.contains(&index) {
            continue;
        }
        let is = rewritten
            .data_resources
            .get(index)
            .ok_or_else(|| format!("bulk data resource {index} left the table"))?;
        let same_entry = (was.serial_size, was.raw_size, was.legacy_bulk_data_flags)
            == (is.serial_size, is.raw_size, is.legacy_bulk_data_flags);
        let kept = same_entry
            && match was.legacy_bulk_data_flags & SEPARATE_PAYLOAD_FLAGS {
                0 => {
                    let old = locate_inline_payload(header, before.exports, index);
                    let new = locate_inline_payload(&rewritten, after.exports, index);
                    match (old, new) {
                        (Some(old), Some(new)) => {
                            old.owner == new.owner
                                && before
                                    .exports
                                    .get(old.start as usize..)
                                    .and_then(|bytes| bytes.get(..old.size as usize))
                                    == after
                                        .exports
                                        .get(new.start as usize..)
                                        .and_then(|bytes| bytes.get(..new.size as usize))
                        }
                        (None, None) => true,
                        _ => false,
                    }
                }
                IN_SEPARATE_FILE if ubulk_after.is_some() => {
                    payload(ubulk_before, was) == payload(ubulk_after, is)
                }
                _ => was.serial_offset == is.serial_offset,
            };
        if !kept {
            return Err(format!(
                "bulk data resource {index}, which is not the texture's, did not keep its bytes"
            ));
        }
    }
    Ok(())
}

/// Refuses a bulk data edit giving a texture's mip bytes of another length: the mip's size is
/// written in the texture's platform data too, and only a texture edit moves both.
pub(crate) fn refuse_mip_resizes(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    edits: &crate::edit::PackageEdits,
) -> Result<(), String> {
    if edits.bulk.is_empty() {
        return Ok(());
    }
    let header = read_header(bundle)?;
    for export in parsed
        .exports
        .iter()
        .filter(|export| is_texture_class(&export.class_name))
    {
        let Ok(layout) = read_texture(bundle, parsed, export.index) else {
            continue;
        };
        for mip in layout.platforms.iter().flat_map(|p| p.mips.iter()) {
            let resized = edits.bulk.iter().any(|edit| {
                edit.resource as i32 == mip.resource
                    && header
                        .data_resources
                        .get(edit.resource as usize)
                        .is_some_and(|resource| resource.serial_size != edit.bytes.len() as i64)
            });
            if resized {
                return Err(format!(
                    "bulk data resource {} is a mip of {}, whose platform data gives its size too: \
                     give the texture new pixels with asset texture --in instead",
                    mip.resource, export.object_name
                ));
            }
        }
    }
    Ok(())
}

/// What a texture edit may not change: every export reads as it did, its values the same but the
/// texture's imported size, and the tables keep their lengths. The pixels were checked by the patch.
pub(crate) fn verify_texture_edit(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &[TextureEdit],
) -> Result<(), String> {
    if before.exports.len() != after.exports.len() || before.imports.len() != after.imports.len() {
        return Err("the tables changed length, which a texture edit does not do".into());
    }
    let values = |export: &ParsedExport| {
        values_but_imported(export, edits.iter().any(|edit| edit.export == export.index))
    };
    for (was, is) in before.exports.iter().zip(&after.exports) {
        if was.class_name != is.class_name
            || std::mem::discriminant(&was.status) != std::mem::discriminant(&is.status)
        {
            return Err(format!(
                "{} reads differently after its texture edit",
                was.path
            ));
        }
        if values(was) != values(is) {
            return Err(format!(
                "{}'s values changed under a texture edit, which changes only pixels",
                was.path
            ));
        }
    }
    Ok(())
}

/// An export's values, wherever they sit, and for a texture given new pixels without the size it
/// records it was imported at, which follows the pixels.
fn values_but_imported(export: &ParsedExport, texture: bool) -> Vec<(String, serde_json::Value)> {
    fn without_spans(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .filter(|(key, _)| key != "span")
                    .map(|(key, value)| (key, without_spans(value)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(without_spans).collect())
            }
            other => other,
        }
    }
    export
        .properties
        .iter()
        .filter(|p| !(texture && p.name == "ImportedSize"))
        .map(|p| {
            (
                p.name.clone(),
                without_spans(serde_json::to_value(&p.value).unwrap_or_default()),
            )
        })
        .collect()
}

/// Gives each texture `edits` names its new pixels, one after another, each read again where the
/// one before it left the package, so the result is what saving them one at a time writes.
/// `textures` are the textures as `bundle` holds them, in the order of `edits`. A texture is given
/// pixels once, and no two may share a payload, which each would write over the other's.
pub(crate) fn patch_textures(
    bundle: &AssetBundle<'_>,
    sidecars: crate::edit::Sidecars<'_>,
    mappings: Option<&Mappings>,
    textures: &[ParsedExport],
    edits: &[TextureEdit],
) -> Result<crate::edit::PatchedBundle, String> {
    let header = read_header(bundle)?;
    let mut owners: std::collections::BTreeMap<usize, u32> = std::collections::BTreeMap::new();
    for (position, (texture, edit)) in textures.iter().zip(edits).enumerate() {
        if edits[..position]
            .iter()
            .any(|other| other.export == edit.export)
        {
            return Err(format!(
                "export {} is given new pixels twice in one save",
                edit.export
            ));
        }
        let layout = read_texture_export(bundle, texture)?;
        for resource in own_resources(&header.data_resources, layout.platform()?, sidecars)? {
            if let Some(other) = owners.insert(resource, edit.export) {
                return Err(format!(
                    "exports {other} and {} share bulk data resource {resource}, so each takes a \
                     save of its own",
                    edit.export
                ));
            }
        }
    }
    let mut out = crate::edit::PatchedBundle {
        asset: bundle.asset.to_vec(),
        exports: bundle.exports.to_vec(),
        applied: Vec::new(),
        bulk: None,
        optional_bulk: None,
        notes: Vec::new(),
    };
    for (position, (texture, edit)) in textures.iter().zip(edits).enumerate() {
        let current = AssetBundle {
            asset: &out.asset,
            exports: &out.exports,
        };
        // The first is read as the caller read it; the rest where the ones before them moved them.
        let moved;
        let texture = if position == 0 {
            texture
        } else {
            moved = crate::package::parse_exports_at(
                &current,
                mappings,
                None,
                ParseOptions::default(),
                &[edit.export],
            )?
            .pop()
            .filter(|moved| moved.properties_end.is_some())
            .ok_or_else(|| {
                format!(
                    "{} does not read whole by itself, so it takes a save of its own",
                    texture.object_name
                )
            })?;
            &moved
        };
        let step = patch_texture(
            &current,
            crate::edit::Sidecars {
                bulk: out.bulk.as_deref().or(sidecars.bulk),
                optional_bulk: sidecars.optional_bulk,
            },
            texture,
            edit,
        )?;
        out.asset = step.asset;
        out.exports = step.exports;
        out.applied.extend(step.applied);
        if step.bulk.is_some() {
            out.bulk = step.bulk;
        }
        // With several textures in the save, each note says which it is about.
        let name = &texture.object_name;
        out.notes.extend(step.notes.into_iter().map(|note| {
            if edits.len() > 1 {
                format!("{name}: {note}")
            } else {
                note
            }
        }));
    }
    Ok(out)
}

/// Gives textures new pixels reading no more of their package than the textures' exports, for a
/// save into a package of thousands of exports: the save is held to the package's bytes by
/// [`verify_texture_save`] rather than to a read of the whole package after it. `None` when a
/// texture does not read whole by itself, or its class is laid out under a name the mappings hold
/// twice, which a read of the whole package could take otherwise: the caller reads it whole.
pub fn patch_textures_alone(
    bundle: &AssetBundle<'_>,
    sidecars: crate::edit::Sidecars<'_>,
    mappings: Option<&Mappings>,
    edits: &crate::edit::PackageEdits,
) -> Result<Option<crate::edit::PatchedBundle>, String> {
    if !edits.only_textures() {
        return Err(
            "only new pixels and nothing else are made from a texture's export alone".into(),
        );
    }
    crate::write::header_round_trips(bundle)?;
    crate::write::check_inline_bulk(bundle)?;
    crate::edit::check_table_expectations(bundle, edits)?;
    let exports: Vec<u32> = edits.textures.iter().map(|edit| edit.export).collect();
    let textures = crate::package::parse_exports_at(
        bundle,
        mappings,
        None,
        ParseOptions::default(),
        &exports,
    )?;
    let whole = textures.iter().all(|texture| {
        matches!(texture.status, ExportStatus::Complete) && texture.properties_end.is_some()
    });
    let twinned = mappings.is_some_and(|mappings| {
        textures.iter().any(|texture| {
            mappings
                .ancestry(&texture.class_name)
                .iter()
                .any(|name| !mappings.twins(name).is_empty())
        })
    });
    if !whole || twinned {
        return Ok(None);
    }
    let patched = patch_textures(bundle, sidecars, mappings, &textures, &edits.textures)?;
    verify_texture_save(bundle, sidecars, &patched, &textures, mappings)?;
    Ok(Some(patched))
}

/// Holds a save of `textures`' new pixels to the bytes it was made from, in place of a read of the
/// whole package after it: the header is the package's own but for the export table's sizes and
/// offsets and the textures' bulk data entries; every other export, and whatever follows the
/// last, holds the bytes it did; each texture's properties hold theirs but for the imported size,
/// which holds the texture's new size where it records one, and read back as they did; and the
/// `.ubulk` holds the payloads its entries name and nothing else. What that leaves a save to
/// change, each texture's platform data and its payloads, the patch checks against the pixels.
pub fn verify_texture_save(
    before: &AssetBundle<'_>,
    sidecars: crate::edit::Sidecars<'_>,
    after: &crate::edit::PatchedBundle,
    textures: &[ParsedExport],
    mappings: Option<&Mappings>,
) -> Result<(), String> {
    let changed = |what: &str| {
        Err(format!(
            "the save of new pixels changed {what}, which new pixels do not"
        ))
    };
    let old = read_header(before)?;
    let rewritten = AssetBundle {
        asset: &after.asset,
        exports: &after.exports,
    };
    let new = read_header(&rewritten)?;
    let total_old = i64::from(old.summary.versioning_info.total_header_size);
    let total_new = i64::from(new.summary.versioning_info.total_header_size);
    if new.exports.len() != old.exports.len()
        || textures
            .iter()
            .any(|texture| texture.index as usize >= old.exports.len())
    {
        return changed("the export table's length");
    }

    // The export table the package had, each export moved by as much as the textures ahead of it
    // grew, the textures at their new sizes.
    let mut expected = old.clone();
    crate::write::to_relative(&mut expected, total_old as usize);
    let starts: Vec<i64> = expected
        .exports
        .iter()
        .map(|export| export.serial_offset)
        .collect();
    for texture in textures {
        let at = texture.index as usize;
        let grown = new.exports[at].serial_size - old.exports[at].serial_size;
        for (position, export) in expected.exports.iter_mut().enumerate() {
            if position == at {
                export.serial_size = new.exports[at].serial_size;
            } else if starts[position] > starts[at] {
                export.serial_offset += grown;
            }
        }
    }

    // Each texture: where it was and is, its entries before and after, and its sizes.
    struct Placed<'t> {
        texture: &'t ParsedExport,
        layout: TextureLayout,
        start_was: usize,
        start_is: usize,
        end_was: usize,
        end_is: usize,
        properties: usize,
        moved_end: u64,
        now: (u32, u32),
    }
    let mut old_own = Vec::new();
    let mut new_own = Vec::new();
    let mut placed = Vec::with_capacity(textures.len());
    for texture in textures {
        let at = texture.index as usize;
        let properties_end = texture
            .properties_end
            .ok_or("a texture's properties did not read whole")?;
        let layout = read_texture_export(before, texture)?;
        old_own.extend(own_resources(
            &old.data_resources,
            layout.platform()?,
            sidecars,
        )?);
        let start_was = usize::try_from(starts[at])
            .map_err(|_| "a texture starts before its package's export data")?;
        let start_is = usize::try_from(expected.exports[at].serial_offset)
            .map_err(|_| "a texture starts before its package's export data")?;
        let properties = usize::try_from(properties_end as i64 - total_old)
            .map_err(|_| "a texture's properties end before its package's export data")?;
        let moved_end =
            u64::try_from(properties as i64 + (start_is as i64 - start_was as i64) + total_new)
                .map_err(|_| "a texture's properties end before the header")?;
        let rebuilt = read_texture_at(&new, &after.exports, at, moved_end, &texture.class_name)?;
        let platform = rebuilt.platform()?;
        new_own.extend(
            platform
                .mips
                .iter()
                .filter_map(|mip| usize::try_from(mip.resource).ok()),
        );
        placed.push(Placed {
            texture,
            start_was,
            start_is,
            end_was: start_was + usize::try_from(old.exports[at].serial_size).unwrap_or(0),
            end_is: start_is + usize::try_from(new.exports[at].serial_size).unwrap_or(0),
            properties,
            moved_end,
            now: (platform.size_x, platform.size_y),
            layout,
        });
    }

    // The bulk data table the package had, but for the textures' entries and where a payload in a
    // rewritten `.ubulk` now sits, its bytes held by the patch.
    let rewrote_ubulk = after.bulk.is_some();
    let mut table = Vec::with_capacity(new.data_resources.len());
    for (index, entry) in new.data_resources.iter().enumerate() {
        if old_own.contains(&index) || new_own.contains(&index) {
            table.push(*entry);
            continue;
        }
        let Some(mut kept) = old.data_resources.get(index).copied() else {
            return changed(&format!("bulk data resource {index}, which no mip holds"));
        };
        if rewrote_ubulk && kept.legacy_bulk_data_flags & SEPARATE_PAYLOAD_FLAGS == IN_SEPARATE_FILE
        {
            kept.serial_offset = entry.serial_offset;
        }
        table.push(kept);
    }
    if let Some(gone) =
        (new.data_resources.len()..old.data_resources.len()).find(|index| !old_own.contains(index))
    {
        return changed(&format!("bulk data resource {gone}, which left the table"));
    }
    expected.data_resources = table;
    let written = crate::write::serialize_header(&expected, total_old as usize)?;
    if written != after.asset {
        let first = written
            .iter()
            .zip(&after.asset)
            .position(|(a, b)| a != b)
            .unwrap_or(written.len().min(after.asset.len()));
        return changed(&format!("the package header, first at {first:#X}"));
    }

    // Every byte outside the textures' exports, which the header says sit where they did.
    let mut spans: Vec<&Placed<'_>> = placed.iter().collect();
    spans.sort_by_key(|place| place.start_was);
    let (mut was, mut is) = (0usize, 0usize);
    for place in &spans {
        if place.start_was < was
            || before.exports.get(was..place.start_was) != after.exports.get(is..place.start_is)
        {
            return changed("the exports between the textures");
        }
        was = place.end_was;
        is = place.end_is;
    }
    if after.exports.get(is..) != before.exports.get(was..) {
        return changed("the exports after the textures, or what follows the last");
    }

    // Each texture's properties: the bytes they were, but for the size a 2D texture records it was
    // imported at, which holds its new size, and they read back as they did.
    for place in &placed {
        let texture = place.texture;
        let mut wanted = before
            .exports
            .get(place.start_was..place.properties)
            .ok_or("a texture's properties end outside it")?
            .to_vec();
        let platform = place.layout.platform()?;
        if (platform.size_x, platform.size_y) != place.now
            && place.layout.class == "Texture2D"
            && let Some((first, end)) = texture
                .properties
                .iter()
                .find(|p| p.name == "ImportedSize")
                .and_then(|p| p.span)
            && end - first == 8
        {
            let offset = usize::try_from(first as i64 - total_old)
                .ok()
                .and_then(|at| at.checked_sub(place.start_was))
                .ok_or("a texture's imported size sits outside it")?;
            wanted[offset..offset + 4].copy_from_slice(&(place.now.0 as i32).to_le_bytes());
            wanted[offset + 4..offset + 8].copy_from_slice(&(place.now.1 as i32).to_le_bytes());
        }
        let length = wanted.len();
        if after.exports.get(place.start_is..place.start_is + length) != Some(wanted.as_slice()) {
            return changed(&format!("{}'s properties", texture.object_name));
        }
    }
    let indices: Vec<u32> = textures.iter().map(|texture| texture.index).collect();
    let reread = crate::package::parse_exports_at(
        &rewritten,
        mappings,
        None,
        ParseOptions::default(),
        &indices,
    )?;
    for (place, reread) in placed.iter().zip(&reread) {
        let texture = place.texture;
        if std::mem::discriminant(&reread.status) != std::mem::discriminant(&texture.status)
            || reread.properties_end != Some(place.moved_end)
            || values_but_imported(reread, true) != values_but_imported(texture, true)
        {
            return changed(&format!("how {}'s properties read", texture.object_name));
        }
    }

    // The `.ubulk`: its entries' payloads end to end, and nothing else.
    if let Some(ubulk) = &after.bulk {
        let mut spans: Vec<(i64, i64)> = new
            .data_resources
            .iter()
            .filter(|resource| {
                resource.legacy_bulk_data_flags & SEPARATE_PAYLOAD_FLAGS == IN_SEPARATE_FILE
                    && resource.serial_size > 0
            })
            .map(|resource| (resource.serial_offset, resource.serial_size))
            .collect();
        spans.sort_unstable();
        let mut next = 0i64;
        for (offset, size) in spans {
            if offset != next {
                return changed("the .ubulk, whose entries no longer follow one another");
            }
            next += size;
        }
        if next != ubulk.len() as i64 {
            return changed("the .ubulk, which holds more than its entries name");
        }
    }
    Ok(())
}

/// Reads the texture back out of the rewritten package and holds every mip to the pixels given.
fn check_pixels(
    after: &AssetBundle<'_>,
    ubulk: Option<&[u8]>,
    texture: &crate::package::ParsedExport,
    total_before: i64,
    edit: &TextureEdit,
) -> Result<(), String> {
    let header = read_header(after)?;
    // A table of another length moves the export data by as much as the header grew.
    let grown = i64::from(header.summary.versioning_info.total_header_size) - total_before;
    let properties_end = texture
        .properties_end
        .and_then(|end| u64::try_from(end as i64 + grown).ok())
        .ok_or("the texture's properties did not read whole")?;
    let layout = read_texture_at(
        &header,
        after.exports,
        edit.export as usize,
        properties_end,
        &texture.class_name,
    )
    .map_err(|e| format!("the rewritten texture does not read back: {e}"))?;
    let platform = layout.platform()?;
    if platform.mips.len() != edit.mips.len() {
        return Err("the rewritten texture has another number of mips".into());
    }
    for (index, (mip, wanted)) in platform.mips.iter().zip(&edit.mips).enumerate() {
        let size = mip.size as usize;
        let bytes = match (mip.placement, mip.inline_at) {
            ("inline", Some(at)) => after.exports.get(at..at + size),
            ("ubulk", _) => {
                ubulk.and_then(|file| file.get(mip.offset as usize..mip.offset as usize + size))
            }
            _ => None,
        };
        if (mip.size_x, mip.size_y, mip.size_z) != (wanted.size_x, wanted.size_y, wanted.size_z)
            || bytes != Some(wanted.bytes.as_slice())
        {
            return Err(format!(
                "mip {index} of the rewritten texture is not the one given"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn resource(flags: u32, offset: i64, size: i64) -> FObjectDataResource {
        FObjectDataResource {
            serial_offset: offset,
            serial_size: size,
            raw_size: size,
            legacy_bulk_data_flags: flags,
            ..Default::default()
        }
    }

    /// A texture's save carries over the `.ubulk`'s other payloads, in file order, only when the
    /// file was read and its entries tile it from its start.
    #[test]
    fn the_ubulk_carries_over_other_payloads_only_when_its_entries_tile_it() {
        let table = [
            resource(IN_SEPARATE_FILE, 10, 20),
            resource(IN_SEPARATE_FILE, 0, 10),
            resource(0, 100, 4),
            resource(IN_SEPARATE_FILE, 30, 0),
            resource(IN_SEPARATE_FILE, 10, 0),
        ];
        let file = [0u8; 30];
        assert_eq!(
            others_in_ubulk(&table, &[1], Some(&file)),
            Ok(vec![4, 0, 3])
        );
        assert_eq!(others_in_ubulk(&table[1..2], &[0], None), Ok(vec![]));
        let unread = others_in_ubulk(&table, &[1], None).unwrap_err();
        assert!(unread.contains("not read"), "{unread}");
        let longer = others_in_ubulk(&table, &[1], Some(&[0u8; 31])).unwrap_err();
        assert!(longer.contains("cover 30 bytes"), "{longer}");
        let mut gap = table;
        gap[0].serial_offset = 12;
        let gap = others_in_ubulk(&gap, &[1], Some(&file)).unwrap_err();
        assert!(gap.contains("does not follow"), "{gap}");
    }
}
