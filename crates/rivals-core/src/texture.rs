//! A texture's pixels: its platform data read from the package, decoded from its pixel format to
//! RGBA, and saved as PNG, or as OpenEXR where they are high dynamic range.

use std::path::Path;

use block_compression::{BC6HSettings, BC7Settings, CompressionVariant};
use half::f16;
use retoc::legacy_asset::FSerializedAssetBundle;
use rivals_uasset::{
    ExportRow, Mappings, PropertyValue, TextureLayout, TextureMip, TexturePlatform,
};
use serde::Serialize;

use crate::asset_edit::AssetEditRequest;

/// How a pixel format stores its pixels.
#[derive(Debug, Clone, Copy)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
    Grey8,
    /// Halves: four channels, RGBA, or one, red.
    Half(u8),
    /// 32-bit floats: four channels, RGBA, or one, red.
    Float(u8),
    Blocks(CompressionVariant),
}

impl PixelFormat {
    /// The format a texture names, or why its pixels cannot be decoded here.
    pub fn of(name: &str) -> Result<Self, String> {
        Ok(match name {
            "PF_B8G8R8A8" => Self::Bgra8,
            "PF_R8G8B8A8" => Self::Rgba8,
            "PF_G8" => Self::Grey8,
            "PF_DXT1" => Self::Blocks(CompressionVariant::BC1),
            "PF_DXT3" => Self::Blocks(CompressionVariant::BC2),
            "PF_DXT5" => Self::Blocks(CompressionVariant::BC3),
            "PF_BC4" => Self::Blocks(CompressionVariant::BC4),
            "PF_BC5" => Self::Blocks(CompressionVariant::BC5),
            "PF_BC7" => Self::Blocks(CompressionVariant::BC7(BC7Settings::alpha_basic())),
            "PF_FloatRGBA" => Self::Half(4),
            "PF_R16F" => Self::Half(1),
            "PF_A32B32G32R32F" => Self::Float(4),
            "PF_R32_FLOAT" => Self::Float(1),
            "PF_BC6H" => Self::Blocks(CompressionVariant::BC6H(BC6HSettings::basic())),
            _ => {
                return Err(format!(
                    "{name} is not a format this decodes: it reads 8-bit color and grey, DXT1, \
                     DXT3, DXT5, BC4, BC5 and BC7, half and float color and red, and BC6H"
                ));
            }
        })
    }

    /// Whether the format holds high dynamic range values, which go out and come in as OpenEXR.
    pub fn is_hdr(self) -> bool {
        matches!(
            self,
            Self::Half(_) | Self::Float(_) | Self::Blocks(CompressionVariant::BC6H(_))
        )
    }

    /// Whether the format holds halves, which its OpenEXR file keeps as halves.
    fn holds_halves(self) -> bool {
        matches!(
            self,
            Self::Half(_) | Self::Blocks(CompressionVariant::BC6H(_))
        )
    }

    /// The bytes one layer of a mip this size takes.
    pub fn mip_size(self, width: u32, height: u32) -> usize {
        let pixels = width as usize * height as usize;
        match self {
            Self::Bgra8 | Self::Rgba8 => pixels * 4,
            Self::Grey8 => pixels,
            Self::Half(channels) => pixels * 2 * usize::from(channels),
            Self::Float(channels) => pixels * 4 * usize::from(channels),
            Self::Blocks(variant) => variant.blocks_byte_size(width, height),
        }
    }

    fn check_size(self, width: u32, height: u32, bytes: &[u8]) -> Result<(), String> {
        let wanted = self.mip_size(width, height);
        if bytes.len() != wanted {
            return Err(format!(
                "a {width}x{height} layer takes {wanted} bytes, not {}",
                bytes.len()
            ));
        }
        Ok(())
    }

    /// One layer's pixels as 8-bit RGBA.
    pub fn decode(self, width: u32, height: u32, bytes: &[u8]) -> Result<Vec<u8>, String> {
        self.check_size(width, height, bytes)?;
        Ok(match self {
            Self::Rgba8 => bytes.to_vec(),
            Self::Bgra8 => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect(),
            Self::Grey8 => bytes.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            Self::Half(_) | Self::Float(_) | Self::Blocks(CompressionVariant::BC6H(_)) => {
                return Err(
                    "the format holds high dynamic range values, which 8 bits cannot".into(),
                );
            }
            Self::Blocks(variant) => {
                // Decoded at whole blocks and cropped: the decoder's partial-block path overruns
                // its buffer on a size that is not a multiple of 4.
                let (padded_x, padded_y) = (width.div_ceil(4) * 4, height.div_ceil(4) * 4);
                let mut padded = vec![0u8; padded_x as usize * padded_y as usize * 4];
                block_compression::decode::decompress_blocks_as_rgba8(
                    variant,
                    padded_x,
                    padded_y,
                    bytes,
                    &mut padded,
                );
                let row = width as usize * 4;
                padded
                    .chunks_exact(padded_x as usize * 4)
                    .take(height as usize)
                    .flat_map(|line| &line[..row])
                    .copied()
                    .collect()
            }
        })
    }

    /// One layer's pixels as RGBA floats. A one-channel format reads as grey, alpha 1; BC6H holds no
    /// alpha either.
    pub fn decode_hdr(self, width: u32, height: u32, bytes: &[u8]) -> Result<Vec<f32>, String> {
        self.check_size(width, height, bytes)?;
        let spread = |channels: u8, values: &mut dyn Iterator<Item = f32>| -> Vec<f32> {
            let values: Vec<f32> = values.collect();
            match channels {
                1 => values.iter().flat_map(|&v| [v, v, v, 1.0]).collect(),
                _ => values,
            }
        };
        Ok(match self {
            Self::Half(channels) => spread(
                channels,
                &mut bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32()),
            ),
            Self::Float(channels) => spread(
                channels,
                &mut bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            ),
            Self::Blocks(CompressionVariant::BC6H(_)) => {
                // Block by block: the crate's whole-image helpers lay a block's three channels out
                // at a four-channel pitch.
                let (width, height) = (width as usize, height as usize);
                let across = width.div_ceil(4);
                let mut rgba = vec![1.0f32; width * height * 4];
                for (index, block) in bytes.as_chunks::<16>().0.iter().enumerate() {
                    let (bx, by) = (index % across * 4, index / across * 4);
                    let mut rgb = [0f32; 48];
                    block_compression::decode::decode_block_bc6h_float(block, &mut rgb, 12, false);
                    for y in 0..4 {
                        for x in 0..4 {
                            if bx + x < width && by + y < height {
                                let at = ((by + y) * width + bx + x) * 4;
                                let from = y * 12 + x * 3;
                                rgba[at..at + 3].copy_from_slice(&rgb[from..from + 3]);
                            }
                        }
                    }
                }
                rgba
            }
            low => low
                .decode(width, height, bytes)?
                .iter()
                .map(|&v| f32::from(v) / 255.0)
                .collect(),
        })
    }
}

/// A channel of the images textures go out and come in as: 8 bits in a PNG, a float in an
/// OpenEXR file.
trait Channel: Copy + Send + Sync + 'static {
    /// What a sum of channel values is held in while they are averaged.
    type Sum: std::iter::Sum;
    fn widen(self) -> Self::Sum;
    /// The mean of `count` values summing to `sum`.
    fn mean(sum: Self::Sum, count: u32) -> Self;
    /// One layer of `format`'s pixels as RGBA in this channel.
    fn decode(
        format: PixelFormat,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> Result<Vec<Self>, String>;
    /// One layer of RGBA in this channel as `format`'s pixels.
    fn encode(
        format: PixelFormat,
        width: u32,
        height: u32,
        rgba: &[Self],
        quality: Quality,
    ) -> Vec<u8>;
}

impl Channel for u8 {
    type Sum = u32;
    fn widen(self) -> u32 {
        u32::from(self)
    }
    fn mean(sum: u32, count: u32) -> u8 {
        ((sum + count / 2) / count) as u8
    }
    fn decode(
        format: PixelFormat,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        format.decode(width, height, bytes)
    }
    fn encode(
        format: PixelFormat,
        width: u32,
        height: u32,
        rgba: &[u8],
        quality: Quality,
    ) -> Vec<u8> {
        format.encode(width, height, rgba, quality)
    }
}

impl Channel for f32 {
    type Sum = f32;
    fn widen(self) -> f32 {
        self
    }
    fn mean(sum: f32, count: u32) -> f32 {
        sum / count as f32
    }
    fn decode(
        format: PixelFormat,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> Result<Vec<f32>, String> {
        format.decode_hdr(width, height, bytes)
    }
    fn encode(
        format: PixelFormat,
        width: u32,
        height: u32,
        rgba: &[f32],
        quality: Quality,
    ) -> Vec<u8> {
        format.encode_hdr(width, height, rgba, quality)
    }
}

#[derive(Debug, Serialize)]
pub struct TextureInfo {
    pub export: u32,
    pub name: String,
    pub layout: TextureLayout,
    /// Mips whose pixels the package does not have: optional ones the game does not install.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub absent_mips: Vec<usize>,
    /// Why its pixels cannot be saved, when they cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undecodable: Option<String>,
    /// Whether its pixels are high dynamic range, which go out and come in as OpenEXR, not PNG.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hdr: bool,
}

/// The texture export `export` names, or the package's first, from its export table.
fn texture_row(rows: &[rivals_uasset::ExportRow], export: Option<u32>) -> Result<u32, String> {
    match export {
        Some(index) => {
            let found = rows
                .get(index as usize)
                .ok_or_else(|| format!("the package has no export {index}"))?;
            if !rivals_uasset::is_texture_class(&found.class_name) {
                return Err(format!(
                    "export {index}, {}, is a {}, not a texture",
                    found.object_name, found.class_name
                ));
            }
            Ok(index)
        }
        None => rows
            .iter()
            .find(|row| rivals_uasset::is_texture_class(&row.class_name))
            .map(|row| row.index)
            .ok_or_else(|| "the package holds no texture".to_string()),
    }
}

/// A texture's package, its name, its path and how it is laid out, from its export alone: a
/// texture's properties are native, so read alone they end where they do in a read of the whole
/// package, and a map's thousands of other exports go unread. One that does not read whole alone
/// is read with the rest of its package.
fn read_layout(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    export: Option<u32>,
) -> Result<(FSerializedAssetBundle, ExportRow, TextureLayout), String> {
    let loaded = crate::asset::load_bundle(
        request.game_root,
        request.container,
        request.entry,
        request.kind,
    )?;
    let mut rows = rivals_uasset::export_rows(&crate::asset_edit::bundle_of(&loaded))?;
    let index = texture_row(&rows, export)?;
    let layout = layout_of(request, mappings, &loaded, index)?;
    let row = rows.swap_remove(index as usize);
    Ok((loaded, row, layout))
}

/// How texture `index` of the package `loaded` holds is laid out, read from its export alone, or
/// with the rest of the package when it does not read whole alone. `request` says where `loaded`
/// was read from.
fn layout_of(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: &FSerializedAssetBundle,
    index: u32,
) -> Result<TextureLayout, String> {
    let bundle = crate::asset_edit::bundle_of(loaded);
    let alone = rivals_uasset::parse_exports_at(
        &bundle,
        mappings,
        None,
        rivals_uasset::ParseOptions::default(),
        &[index],
    )?;
    match alone
        .first()
        .filter(|export| export.properties_end.is_some())
    {
        Some(export) => rivals_uasset::read_texture_export(&bundle, export),
        None => {
            let parsed = crate::asset_edit::parse_loaded(request, mappings, loaded)?;
            rivals_uasset::read_texture(&bundle, &parsed, index)
        }
    }
}

/// A texture's format, size and mips, and whether its pixels can be saved, as PNG or OpenEXR.
pub fn texture_info(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    export: Option<u32>,
) -> Result<TextureInfo, String> {
    let (loaded, row, layout) = read_layout(request, mappings, export)?;
    let name = row.object_name;
    let format = layout
        .platform()
        .and_then(|platform| PixelFormat::of(&platform.format));
    let hdr = format.as_ref().is_ok_and(|format| format.is_hdr());
    let undecodable = format.err();
    let absent_mips = layout
        .platforms
        .first()
        .map(|platform| absent_mips(&loaded, platform))
        .unwrap_or_default();
    Ok(TextureInfo {
        export: layout.export,
        name,
        layout,
        absent_mips,
        undecodable,
        hdr,
    })
}

/// The bytes of `mip`, from the file the bulk data table puts them in.
fn mip_bytes<'a>(loaded: &'a FSerializedAssetBundle, mip: &TextureMip) -> Result<&'a [u8], String> {
    let size = usize::try_from(mip.size).map_err(|_| "a mip has no size".to_string())?;
    let (file, at): (Option<&[u8]>, i64) = match mip.placement {
        "inline" => (
            Some(&loaded.exports_file_buffer),
            mip.inline_at.map_or(-1, |at| at as i64),
        ),
        "ubulk" => (loaded.bulk_data_buffer.as_deref(), mip.offset),
        "uptnl" => (loaded.optional_bulk_data_buffer.as_deref(), mip.offset),
        _ => (loaded.memory_mapped_bulk_data_buffer.as_deref(), mip.offset),
    };
    let file = file.ok_or_else(|| {
        if mip.placement == "uptnl" {
            "its pixels are optional data the game does not install, so the package does not have \
             them"
                .to_string()
        } else {
            format!(
                "its pixels are in the package's .{}, which was not found beside it",
                mip.placement
            )
        }
    })?;
    usize::try_from(at)
        .ok()
        .and_then(|at| file.get(at..at + size))
        .ok_or_else(|| format!("its pixels run past the end of its .{}", mip.placement))
}

/// The mips whose pixels are not in the package.
fn absent_mips(loaded: &FSerializedAssetBundle, platform: &TexturePlatform) -> Vec<usize> {
    platform
        .mips
        .iter()
        .enumerate()
        .filter(|(_, mip)| mip_bytes(loaded, mip).is_err())
        .map(|(index, _)| index)
        .collect()
}

/// A mip's layers, the six faces of a cube or the slices of an array or volume, decoded and
/// stacked top to bottom.
fn decode_mip<C: Channel>(
    format: PixelFormat,
    platform: &TexturePlatform,
    mip: &TextureMip,
    bytes: &[u8],
) -> Result<(u32, u32, Vec<C>), String> {
    let layer = format.mip_size(mip.size_x, mip.size_y);
    if layer == 0 || !bytes.len().is_multiple_of(layer) {
        return Err(format!(
            "the mip holds {} bytes, which is not a whole number of {}x{} {} layers",
            bytes.len(),
            mip.size_x,
            mip.size_y,
            platform.format
        ));
    }
    let layers = bytes.len() / layer;
    let mut rgba = Vec::with_capacity(mip.size_x as usize * mip.size_y as usize * 4 * layers);
    for chunk in bytes.chunks_exact(layer) {
        rgba.extend(C::decode(format, mip.size_x, mip.size_y, chunk)?);
    }
    Ok((mip.size_x, mip.size_y * layers as u32, rgba))
}

/// 8-bit RGBA pixels as a PNG file's bytes.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Could not write the PNG: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("Could not write the PNG: {e}"))?;
    }
    Ok(out)
}

/// RGBA floats as an OpenEXR file's bytes: one RGBA layer of scan lines, ZIP-compressed, which
/// every tool reads, in halves for a texture that holds halves and in floats for one that holds
/// floats.
pub fn encode_exr(width: u32, height: u32, rgba: &[f32], halves: bool) -> Result<Vec<u8>, String> {
    use exr::prelude::*;

    let (w, h) = (width as usize, height as usize);
    let pixel = |x: usize, y: usize| {
        let at = (y * w + x) * 4;
        (rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3])
    };
    let encoding = Encoding {
        compression: Compression::ZIP16,
        blocks: Blocks::ScanLines,
        line_order: LineOrder::Increasing,
    };
    let mut out = std::io::Cursor::new(Vec::new());
    let written = if halves {
        let channels = SpecificChannels::rgba(|Vec2(x, y)| {
            let (r, g, b, a) = pixel(x, y);
            (
                f16::from_f32(r),
                f16::from_f32(g),
                f16::from_f32(b),
                f16::from_f32(a),
            )
        });
        Image::from_encoded_channels((w, h), encoding, channels)
            .write()
            .to_buffered(&mut out)
    } else {
        let channels = SpecificChannels::rgba(|Vec2(x, y)| pixel(x, y));
        Image::from_encoded_channels((w, h), encoding, channels)
            .write()
            .to_buffered(&mut out)
    };
    written.map_err(|e| format!("Could not write the OpenEXR file: {e}"))?;
    Ok(out.into_inner())
}

/// An OpenEXR file's first RGBA layer, at its largest level, as floats: alpha 1 where it has none.
pub fn decode_exr(bytes: &[u8]) -> Result<(u32, u32, Vec<f32>), String> {
    use exr::prelude::*;

    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(
            |size: Vec2<usize>, _: &RgbaChannels| (size.width(), vec![0f32; size.area() * 4]),
            |(width, pixels): &mut (usize, Vec<f32>),
             Vec2(x, y): Vec2<usize>,
             (r, g, b, a): (f32, f32, f32, f32)| {
                let at = (y * *width + x) * 4;
                pixels[at..at + 4].copy_from_slice(&[r, g, b, a]);
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_buffered(std::io::Cursor::new(bytes))
        .map_err(|e| format!("Could not read the OpenEXR file: {e}"))?;
    let size = image.layer_data.size;
    if size.area() == 0 {
        return Err("the OpenEXR file has no pixels".into());
    }
    let (_, pixels) = image.layer_data.channel_data.pixels;
    Ok((size.width() as u32, size.height() as u32, pixels))
}

/// An image file's pixels as RGBA: 8 bits from a PNG, floats from an OpenEXR file.
enum Pixels {
    Low(Vec<u8>),
    High(Vec<f32>),
}

/// The image at `path`, a PNG or an OpenEXR file told apart by how it starts, with its size.
fn read_image(path: &Path) -> Result<(u32, u32, Pixels), String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        let (width, height, rgba) = decode_exr(&bytes)?;
        Ok((width, height, Pixels::High(rgba)))
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (width, height, rgba) = decode_png(&bytes)?;
        Ok((width, height, Pixels::Low(rgba)))
    } else {
        Err(format!(
            "{} is neither a PNG nor an OpenEXR file",
            path.display()
        ))
    }
}

/// Why an image of the other kind is refused: a texture holding high dynamic range values goes out
/// and comes in as OpenEXR, an 8-bit one as PNG.
fn other_kind(name: &str, format: &str, hdr: bool, verb: &str) -> String {
    if hdr {
        format!(
            "{name} is {format}, whose high dynamic range values a PNG cannot hold: {verb} an \
             OpenEXR (.exr) file"
        )
    } else {
        format!("{name} is {format}, whose values are 8-bit: {verb} a PNG")
    }
}

#[derive(Debug, Serialize)]
pub struct SavedImage {
    pub path: String,
    pub export: u32,
    pub mip: usize,
    pub format: String,
    pub width: u32,
    pub height: u32,
}

/// Saves mip `mip` of a texture at `out`, or with `None` the largest the package has: as a PNG, or
/// as an OpenEXR file for a texture holding high dynamic range values, which `out` is named for.
pub fn save_image(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    export: Option<u32>,
    mip: Option<usize>,
    out: &Path,
) -> Result<SavedImage, String> {
    let (loaded, row, layout) = read_layout(request, mappings, export)?;
    let name = row.object_name;
    let platform = layout.platform()?;
    let format = PixelFormat::of(&platform.format)?;
    let exr = out
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exr"));
    if exr != format.is_hdr() {
        return Err(other_kind(
            &name,
            &platform.format,
            format.is_hdr(),
            "save it as",
        ));
    }
    let mip = match mip {
        Some(mip) => mip,
        None => (0..platform.mips.len())
            .find(|index| mip_bytes(&loaded, &platform.mips[*index]).is_ok())
            .ok_or("the package has the pixels of none of the texture's mips")?,
    };
    let chosen = platform.mips.get(mip).ok_or_else(|| {
        format!(
            "the texture has {} mip(s), counted from 0",
            platform.mips.len()
        )
    })?;
    let bytes = mip_bytes(&loaded, chosen).map_err(|e| format!("mip {mip}: {e}"))?;
    let (width, height, file) = if exr {
        let (width, height, rgba) = decode_mip::<f32>(format, platform, chosen, bytes)?;
        let file = encode_exr(width, height, &rgba, format.holds_halves())?;
        (width, height, file)
    } else {
        let (width, height, rgba) = decode_mip::<u8>(format, platform, chosen, bytes)?;
        (width, height, encode_png(width, height, &rgba)?)
    };
    std::fs::write(out, file).map_err(|e| format!("Could not write {}: {e}", out.display()))?;
    Ok(SavedImage {
        path: out.to_string_lossy().into_owned(),
        export: layout.export,
        mip,
        format: platform.format.clone(),
        width,
        height,
    })
}

/// How hard the BC7 encoder works: harder finds closer colors, slower.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Fast,
    #[default]
    Default,
    Best,
}

impl Quality {
    /// The quality an edit file names, `default` when it names none.
    pub fn named(name: Option<&str>) -> Result<Self, String> {
        match name.map(str::trim) {
            None | Some("default") => Ok(Self::Default),
            Some("fast") => Ok(Self::Fast),
            Some("best") => Ok(Self::Best),
            Some(other) => Err(format!("quality {other} is none of fast, default and best")),
        }
    }
}

fn bc7_settings(quality: Quality, alpha: bool) -> BC7Settings {
    match (quality, alpha) {
        (Quality::Fast, true) => BC7Settings::alpha_very_fast(),
        (Quality::Fast, false) => BC7Settings::opaque_very_fast(),
        (Quality::Default, true) => BC7Settings::alpha_basic(),
        (Quality::Default, false) => BC7Settings::opaque_basic(),
        (Quality::Best, true) => BC7Settings::alpha_slow(),
        (Quality::Best, false) => BC7Settings::opaque_slow(),
    }
}

/// `rgba` padded out to whole 4x4 blocks by repeating its last column and row.
fn pad_to_blocks<T: Copy>(width: u32, height: u32, rgba: &[T]) -> (u32, u32, Vec<T>) {
    let (padded_x, padded_y) = (width.div_ceil(4) * 4, height.div_ceil(4) * 4);
    if (padded_x, padded_y) == (width, height) {
        return (width, height, rgba.to_vec());
    }
    let mut out = Vec::with_capacity(padded_x as usize * padded_y as usize * 4);
    for y in 0..padded_y {
        let row = y.min(height - 1) as usize * width as usize * 4;
        for x in 0..padded_x {
            let at = row + x.min(width - 1) as usize * 4;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    (padded_x, padded_y, out)
}

/// One layer of RGBA encoded as `variant`'s blocks, `compress` taking it a band of rows at a time,
/// padded out to whole blocks. Blocks are stored a row of them at a time, so bands encode apart,
/// each on a thread of its own: one large mip is most of a texture's work.
fn encode_blocks<T: Copy + Send + Sync>(
    variant: CompressionVariant,
    width: u32,
    height: u32,
    rgba: &[T],
    compress: impl Fn(CompressionVariant, &[T], &mut [u8], u32, u32, u32) + Sync,
) -> Vec<u8> {
    use rayon::prelude::*;

    let (padded_x, padded_y, padded) = pad_to_blocks(width, height, rgba);
    let mut blocks = vec![0u8; variant.blocks_byte_size(padded_x, padded_y)];
    const BAND_ROWS: usize = 16;
    let pixel_band = padded_x as usize * 4 * 4 * BAND_ROWS;
    let block_band = variant.bytes_per_row(padded_x) as usize * BAND_ROWS;
    blocks
        .par_chunks_mut(block_band)
        .zip(padded.par_chunks(pixel_band))
        .for_each(|(out, pixels)| {
            let rows = (pixels.len() / (padded_x as usize * 4)) as u32;
            compress(variant, pixels, out, padded_x, rows, padded_x * 4);
        });
    blocks
}

impl PixelFormat {
    /// One layer of 8-bit RGBA pixels in this format.
    pub fn encode(self, width: u32, height: u32, rgba: &[u8], quality: Quality) -> Vec<u8> {
        match self {
            Self::Rgba8 => rgba.to_vec(),
            Self::Bgra8 => rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect(),
            Self::Grey8 => rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| {
                    ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000)
                        as u8
                })
                .collect(),
            Self::Half(_) | Self::Float(_) | Self::Blocks(CompressionVariant::BC6H(_)) => {
                let rgba: Vec<f32> = rgba.iter().map(|&v| f32::from(v) / 255.0).collect();
                self.encode_hdr(width, height, &rgba, quality)
            }
            Self::Blocks(variant) => {
                let variant = match variant {
                    CompressionVariant::BC7(_) => CompressionVariant::BC7(bc7_settings(
                        quality,
                        rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255),
                    )),
                    other => other,
                };
                encode_blocks(
                    variant,
                    width,
                    height,
                    rgba,
                    block_compression::encode::compress_rgba8,
                )
            }
        }
    }

    /// One layer of RGBA floats in this format. A one-channel format takes red. BC6H holds no
    /// alpha and no negatives, so they are left out and clamped to 0.
    pub fn encode_hdr(self, width: u32, height: u32, rgba: &[f32], quality: Quality) -> Vec<u8> {
        let channels = |count: u8| {
            rgba.as_chunks::<4>()
                .0
                .iter()
                .flat_map(move |pixel| pixel[..usize::from(count)].iter().copied())
        };
        match self {
            Self::Half(count) => channels(count)
                .flat_map(|v| f16::from_f32(v).to_le_bytes())
                .collect(),
            Self::Float(count) => channels(count).flat_map(f32::to_le_bytes).collect(),
            Self::Blocks(CompressionVariant::BC6H(_)) => {
                let settings = match quality {
                    Quality::Fast => BC6HSettings::very_fast(),
                    Quality::Default => BC6HSettings::basic(),
                    Quality::Best => BC6HSettings::slow(),
                };
                // The encoder reads a half's bits as an unsigned number, so a negative, an infinity
                // or a NaN would come out as a large value.
                let largest = f16::MAX.to_f32();
                let halves: Vec<f16> = rgba
                    .iter()
                    .map(|&v| {
                        f16::from_f32(if v.is_nan() {
                            0.0
                        } else {
                            v.clamp(0.0, largest)
                        })
                    })
                    .collect();
                encode_blocks(
                    CompressionVariant::BC6H(settings),
                    width,
                    height,
                    &halves,
                    block_compression::encode::compress_rgba16,
                )
            }
            low => {
                let rgba: Vec<u8> = rgba
                    .iter()
                    .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    .collect();
                low.encode(width, height, &rgba, quality)
            }
        }
    }
}

/// A PNG's pixels as 8-bit RGBA, whatever its color type and depth.
pub fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("Could not read the PNG: {e}"))?;
    let size = reader
        .output_buffer_size()
        .ok_or("the PNG is too large to read")?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| format!("Could not read the PNG: {e}"))?;
    buffer.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("the PNG's palette did not expand".into()),
    };
    if info.width == 0 || info.height == 0 {
        return Err("the PNG has no pixels".into());
    }
    Ok((info.width, info.height, rgba))
}

/// The next mip down of `layers`, each `width` by `height`: half the size, each pixel the average
/// of the two by two it covers in every one of them. One layer halves a 2D image; two adjacent
/// slices of a volume halve it in depth too.
fn half_of<C: Channel>(width: u32, height: u32, layers: &[&[C]]) -> (u32, u32, Vec<C>) {
    let (next_x, next_y) = ((width / 2).max(1), (height / 2).max(1));
    let at = |x: u32, y: u32| {
        (y.min(height - 1) as usize * width as usize + x.min(width - 1) as usize) * 4
    };
    let samples = 4 * layers.len() as u32;
    let mut out = Vec::with_capacity(next_x as usize * next_y as usize * 4);
    for y in 0..next_y {
        for x in 0..next_x {
            let corners = [
                at(x * 2, y * 2),
                at(x * 2 + 1, y * 2),
                at(x * 2, y * 2 + 1),
                at(x * 2 + 1, y * 2 + 1),
            ];
            for channel in 0..4 {
                let sum = layers
                    .iter()
                    .flat_map(|layer| corners.iter().map(|&c| layer[c + channel].widen()))
                    .sum();
                out.push(C::mean(sum, samples));
            }
        }
    }
    (next_x, next_y, out)
}

/// The next mip down of a 2D image.
fn half<C: Channel>(width: u32, height: u32, rgba: &[C]) -> (u32, u32, Vec<C>) {
    half_of(width, height, &[rgba])
}

/// One mip of a texture as layers: a 2D texture's one, a cube's six faces, an array's slices or a
/// volume's depth, each `width` by `height`.
struct Level<C> {
    width: u32,
    height: u32,
    layers: Vec<Vec<C>>,
}

impl<C: Channel> Level<C> {
    /// The next mip down: each layer halved on its own, or for a volume pairs of them averaged
    /// into one, so its depth halves too.
    fn half(&self, volume: bool) -> Level<C> {
        let halved: Vec<(u32, u32, Vec<C>)> = if volume {
            let depth = self.layers.len();
            (0..(depth / 2).max(1))
                .map(|k| {
                    let pair = [
                        &self.layers[2 * k][..],
                        &self.layers[(2 * k + 1).min(depth - 1)],
                    ];
                    half_of(self.width, self.height, &pair)
                })
                .collect()
        } else {
            self.layers
                .iter()
                .map(|layer| half(self.width, self.height, layer))
                .collect()
        };
        let (width, height) = halved
            .first()
            .map_or((1, 1), |(width, height, _)| (*width, *height));
        Level {
            width,
            height,
            layers: halved.into_iter().map(|(_, _, layer)| layer).collect(),
        }
    }
}

/// How many mips a texture whose largest stored mip is this size gets: one if the source had one,
/// else a chain as long as a full one less as many as the source's left off its small end. The
/// chain is counted from the largest mip stored, so mips a cook left off the large end are not
/// taken for ones missing from the small end. A volume's depth counts toward its chain; an array's
/// slice count does not.
fn mip_count(source: &TexturePlatform, volume: bool, size: (u32, u32, u32)) -> usize {
    let full = |x: u32, y: u32, z: u32| {
        let z = if volume { z } else { 1 };
        (32 - x.max(y).max(z).max(1).leading_zeros()) as usize
    };
    let Some(top) = source.mips.first() else {
        return 1;
    };
    if source.mips.len() == 1 {
        return 1;
    }
    let short = full(top.size_x, top.size_y, top.size_z).saturating_sub(source.mips.len());
    full(size.0, size.1, size.2).saturating_sub(short).max(1)
}

/// How many layers texture `layout` stacks in its image, top to bottom: a cube's six faces, an
/// array's slices or a volume's depth, else one.
fn layer_count(layout: &TextureLayout, platform: &TexturePlatform) -> u32 {
    match layout.class.as_str() {
        "TextureCube" => 6,
        "Texture2DArray" | "VolumeTexture" => platform.slices.max(1),
        _ => 1,
    }
}

/// What texture `layout`'s layers are called in a message.
fn layer_noun(layout: &TextureLayout) -> &'static str {
    match layout.class.as_str() {
        "TextureCube" => "faces",
        "VolumeTexture" => "depth slices",
        _ => "slices",
    }
}

/// Texture `layout`'s mips in `format`, built from an image `width` by `stacked` holding its layers
/// top to bottom: a cube's six faces, an array's slices or a volume's depth, as many as it has. The
/// chain is as long as [`mip_count`] makes it.
fn build_mips<C: Channel>(
    layout: &TextureLayout,
    format: PixelFormat,
    width: u32,
    stacked: u32,
    rgba: &[C],
    quality: Quality,
) -> Result<Vec<rivals_uasset::NewMip>, String> {
    let platform = layout.platform()?;
    let layers = layer_count(layout, platform);
    if !stacked.is_multiple_of(layers) {
        return Err(format!(
            "it has {layers} {} stacked top to bottom, so the image's height has to divide by \
             {layers}; it is {width}x{stacked}",
            layer_noun(layout)
        ));
    }
    let height = stacked / layers;
    if layout.class == "TextureCube" && width != height {
        return Err(format!(
            "a cube's faces are square, so the image is as wide as one face and 6 times as tall; \
             it is {width}x{stacked}"
        ));
    }
    let volume = layout.class == "VolumeTexture";
    let layer_bytes = width as usize * height as usize * 4;
    if rgba.len() != layer_bytes * layers as usize {
        return Err(format!(
            "the image holds {} values, not the {} of {width}x{stacked} RGBA",
            rgba.len(),
            layer_bytes * layers as usize
        ));
    }
    let mut levels = vec![Level {
        width,
        height,
        layers: rgba.chunks_exact(layer_bytes).map(<[C]>::to_vec).collect(),
    }];
    let count = mip_count(platform, volume, (width, height, layers));
    while levels.len() < count {
        let next = levels.last().ok_or("no mips")?.half(volume);
        levels.push(next);
    }
    use rayon::prelude::*;
    Ok(levels
        .par_iter()
        .map(|level| rivals_uasset::NewMip {
            size_x: level.width,
            size_y: level.height,
            // A cube records a depth of 1 and an array its slice count at every mip.
            size_z: match layout.class.as_str() {
                "Texture2DArray" | "VolumeTexture" => level.layers.len() as u32,
                _ => 1,
            },
            bytes: level
                .layers
                .iter()
                .flat_map(|layer| C::encode(format, level.width, level.height, layer, quality))
                .collect(),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub export: u32,
    pub name: String,
    pub format: String,
    pub from: (u32, u32),
    pub to: (u32, u32),
    /// The layers each mip holds, stacked in the image: a cube's 6 faces, an array's slices or a
    /// volume's depth, else 1.
    pub layers: u32,
    pub mips: usize,
    /// Brushes in the mod drawing the texture at a size they store, which a new size leaves as is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub brushes: Vec<String>,
    pub warnings: Vec<String>,
    /// What the save wrote, or `None` for a dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<String>,
}

/// What [`import_images`] made of each image, and what its save wrote.
#[derive(Debug, Serialize)]
pub struct ImportBatch {
    pub textures: Vec<ImportReport>,
    /// Brushes in the mod drawing one of the textures at a size they store, which a new size
    /// leaves as is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub brushes: Vec<String>,
    /// What the save had to say, beside each texture's own warnings.
    pub warnings: Vec<String>,
    /// What the save wrote, or `None` for a dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<String>,
}

/// Gives a texture the pixels of the image at `image`, encoded in the texture's own format at the
/// image's size, its mips rebuilt from them: a PNG for an 8-bit texture, an OpenEXR file for one
/// holding high dynamic range values. A cube's six faces, an array's slices or a volume's depth
/// stack top to bottom in the image, as a saved one has them, and keep their count.
#[allow(clippy::too_many_arguments)]
pub fn import_image(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    export: Option<u32>,
    image: &Path,
    quality: Quality,
    options: &crate::asset_edit::SaveOptions,
    dry_run: bool,
) -> Result<ImportReport, String> {
    let ImportBatch {
        textures,
        brushes,
        warnings,
        written,
    } = import_images(
        request,
        mappings,
        &[(export, image)],
        quality,
        options,
        dry_run,
    )?;
    let mut report = textures
        .into_iter()
        .next()
        .ok_or("no texture was given an image")?;
    report.warnings.extend(warnings);
    report.brushes = brushes;
    report.written = written;
    Ok(report)
}

/// Gives each texture the pixels of its image, as [`import_image`] gives one, in one save: the
/// package is read once, each image encoded in turn, and the mod written once. `export` `None`
/// is the package's first texture.
pub fn import_images(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    images: &[(Option<u32>, &Path)],
    quality: Quality,
    options: &crate::asset_edit::SaveOptions,
    dry_run: bool,
) -> Result<ImportBatch, String> {
    // Read where the save reads, and the save held to the exports found there.
    let layered = crate::asset_edit::layered_read(request, options)?;
    let from = match &layered {
        Some((container, entry, kind)) => AssetEditRequest {
            container,
            entry,
            kind: *kind,
            changes: rivals_uasset::PackageEdits::default(),
            ..*request
        },
        None => AssetEditRequest {
            changes: rivals_uasset::PackageEdits::default(),
            ..*request
        },
    };
    let loaded = crate::asset::load_bundle(from.game_root, from.container, from.entry, from.kind)?;
    let rows = rivals_uasset::export_rows(&crate::asset_edit::bundle_of(&loaded))?;
    let mut batch = ImportBatch {
        textures: Vec::with_capacity(images.len()),
        brushes: Vec::new(),
        warnings: Vec::new(),
        written: None,
    };
    let mut textures: Vec<rivals_uasset::TextureEdit> = Vec::with_capacity(images.len());
    let mut expect = rivals_uasset::Expected::default();
    for &(export, image) in images {
        let index = texture_row(&rows, export)?;
        let row = &rows[index as usize];
        if textures.iter().any(|texture| texture.export == index) {
            return Err(format!(
                "{} is given two images; give each texture one",
                row.object_name
            ));
        }
        let layout = layout_of(&from, mappings, &loaded, index)?;
        let (report, mips) = encode_image(&loaded, row, &layout, image, quality)?;
        batch.textures.push(report);
        expect.exports.insert(index, row.path.clone());
        textures.push(rivals_uasset::TextureEdit {
            export: index,
            mips,
        });
    }
    if dry_run {
        return Ok(batch);
    }
    let resized = batch.textures.iter().any(|report| report.from != report.to);
    let save = AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: request.kind,
        mod_name: request.mod_name,
        changes: rivals_uasset::PackageEdits {
            textures,
            expect,
            allow_unchecked: request.changes.allow_unchecked,
            ..Default::default()
        },
    };
    match crate::asset_edit::save_edits(&save, mappings, options)? {
        crate::asset_edit::SaveOutcome::Written {
            message,
            warnings,
            pak,
        } => {
            batch.written = Some(message);
            batch.warnings = warnings;
            if resized && let Some(texture) = crate::asset::package_name_of_entry(request.entry) {
                batch.brushes = brushes_at(request.game_root, &pak, mappings, &texture);
            }
        }
        crate::asset_edit::SaveOutcome::HoldsCopy { pak } => {
            return Err(format!(
                "{pak} already holds an edited copy of {}; leave out --from to build on it, or pass --overwrite to start again from the source",
                request.entry
            ));
        }
    }
    Ok(batch)
}

/// The image at `image` encoded as new mips for texture `row` laid out as `layout`, in the
/// package `loaded` holds, with what that makes of it.
fn encode_image(
    loaded: &FSerializedAssetBundle,
    row: &ExportRow,
    layout: &TextureLayout,
    image: &Path,
    quality: Quality,
) -> Result<(ImportReport, Vec<rivals_uasset::NewMip>), String> {
    let name = &row.object_name;
    if let Some(reason) = rivals_uasset::texture_save_lock(
        &crate::asset_edit::bundle_of(loaded),
        layout,
        rivals_uasset::Sidecars {
            bulk: loaded.bulk_data_buffer.as_deref(),
            optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
        },
    ) {
        return Err(format!("{name}'s pixels cannot be replaced: {reason}"));
    }
    let platform = layout.platform()?;
    let format = PixelFormat::of(&platform.format)?;
    let (width, stacked, pixels) = read_image(image)?;
    let mips = match &pixels {
        Pixels::High(rgba) if format.is_hdr() => {
            build_mips(layout, format, width, stacked, rgba, quality)
        }
        Pixels::Low(rgba) if !format.is_hdr() => {
            build_mips(layout, format, width, stacked, rgba, quality)
        }
        _ => Err(other_kind(
            name,
            &platform.format,
            format.is_hdr(),
            "give it",
        )),
    }
    .map_err(|e| {
        if e.starts_with(name.as_str()) {
            e
        } else {
            format!("{name}: {e}")
        }
    })?;
    let (height, layers) = (mips[0].size_y, layer_count(layout, platform));

    // Sizes are the texture's own, which a cook that left out its largest mips keeps full.
    let to = platform.full_size((width, height));
    let left_out = (platform.first_mip > 0).then(|| {
        format!(
            "the cook leaves out {name}'s {} largest mip(s), so the image is mip {} of a {}x{} texture",
            platform.first_mip, platform.first_mip, to.0, to.1
        )
    });
    let report = ImportReport {
        export: layout.export,
        name: name.clone(),
        format: platform.format.clone(),
        from: (platform.size_x, platform.size_y),
        to,
        layers,
        mips: mips.len(),
        brushes: Vec::new(),
        warnings: left_out.into_iter().collect(),
        written: None,
    };
    Ok((report, mips))
}

/// `request`'s edits with each image they name encoded as its texture's new pixels, against the
/// package `loaded` holds, the one the save reads: each image fits whatever copy the save builds
/// on, and the save is held to each texture being where it was found.
pub(crate) fn encode_images(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: &FSerializedAssetBundle,
) -> Result<rivals_uasset::PackageEdits, String> {
    let mut changes = request.changes.clone();
    let images = std::mem::take(&mut changes.texture_images);
    if images.is_empty() {
        return Ok(changes);
    }
    let rows = rivals_uasset::export_rows(&crate::asset_edit::bundle_of(loaded))?;
    for image in images {
        let named = match &image.object {
            Some(object) => rivals_uasset::export_row_of(&rows, object)?.index,
            None => image.export,
        };
        let index = texture_row(&rows, Some(named))?;
        let row = &rows[index as usize];
        if changes
            .textures
            .iter()
            .any(|texture| texture.export == index)
        {
            return Err(format!(
                "{} is given two images; give each texture one",
                row.object_name
            ));
        }
        let quality = Quality::named(image.quality.as_deref())?;
        let layout = layout_of(request, mappings, loaded, index)?;
        let (_, mips) = encode_image(loaded, row, &layout, Path::new(&image.image), quality)?;
        changes.expect.exports.insert(index, row.path.clone());
        changes.textures.push(rivals_uasset::TextureEdit {
            export: index,
            mips,
        });
    }
    Ok(changes)
}

/// The number a stored field holds, where it holds one.
fn number_of(value: &PropertyValue) -> Option<f64> {
    match value {
        PropertyValue::Float { value } => Some(*value),
        PropertyValue::Int { value } => Some(*value as f64),
        _ => None,
    }
}

/// Every brush under `value` that draws `texture` at a size it stores, by the path to it.
fn brushes_in(value: &PropertyValue, path: &str, texture: &str, out: &mut Vec<String>) {
    match value {
        PropertyValue::Struct { fields, .. } => {
            let draws = fields.iter().any(|field| {
                field.name == "ResourceObject"
                    && matches!(&field.value, PropertyValue::Object { path: Some(object), .. }
                        if object.split('.').next() == Some(texture))
            });
            let stored = fields.iter().find_map(|field| match &field.value {
                PropertyValue::Struct { fields: xy, .. } if field.name == "ImageSize" => {
                    let xy: Vec<f64> = xy.iter().filter_map(|f| number_of(&f.value)).collect();
                    (xy.len() == 2).then(|| (xy[0], xy[1]))
                }
                _ => None,
            });
            if let (true, Some((x, y))) = (draws, stored) {
                out.push(format!("{path}, at {x}x{y}"));
            }
            for field in fields {
                brushes_in(
                    &field.value,
                    &format!("{path}.{}", field.name),
                    texture,
                    out,
                );
            }
        }
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            for (index, item) in items.iter().enumerate() {
                brushes_in(item, &format!("{path}[{index}]"), texture, out);
            }
        }
        PropertyValue::Map { entries } => {
            for (index, entry) in entries.iter().enumerate() {
                brushes_in(&entry.value, &format!("{path}[{index}]"), texture, out);
            }
        }
        _ => {}
    }
}

/// The brushes in the mod beside `pak` that draw `texture` at a size they store: given another
/// size, it still draws at that one through them.
fn brushes_at(
    game_root: &str,
    pak: &Path,
    mappings: Option<&Mappings>,
    texture: &str,
) -> Vec<String> {
    use crate::schema_synth::{self, LayoutReader, PackageSource};

    let utoc = pak.with_extension("utoc").to_string_lossy().into_owned();
    let Ok((store, packages)) = crate::asset::list_packages(game_root, &utoc) else {
        return Vec::new();
    };
    let converter = crate::asset::PackageConverter::new(store.as_ref());
    let layouts = LayoutReader::through(store.as_ref(), &converter);
    // A brush names the texture by a reference its package imports, so only those packages, and
    // the texture's own, are read.
    let drawn = crate::asset::package_id(texture);
    let mut out = Vec::new();
    for (id, entry) in &packages {
        let imports = *id == drawn
            || store
                .package_store_entry(*id)
                .is_some_and(|held| held.imported_packages.contains(&drawn));
        if !imports {
            continue;
        }
        let Ok(bundle) = converter.convert(*id, entry) else {
            continue;
        };
        let parsed = schema_synth::parse_package_through(
            &rivals_uasset::AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            },
            mappings,
            &PackageSource {
                game_root,
                container: &utoc,
                entry,
                kind: crate::asset::AssetSource::Utoc,
                mod_container: None,
            },
            &layouts,
            rivals_uasset::ParseOptions::default(),
        );
        let Ok(parsed) = parsed else {
            continue;
        };
        for export in &parsed.exports {
            for property in &export.properties {
                brushes_in(
                    &property.value,
                    &format!("{entry}: {}.{}", export.object_name, property.name),
                    texture,
                    &mut out,
                );
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn png_pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().expect("info");
        let mut buf = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn bgra_pixels_save_as_the_same_colors_in_rgba() {
        let bgra = [10, 20, 30, 255, 1, 2, 3, 4];
        let rgba = PixelFormat::Bgra8.decode(2, 1, &bgra).unwrap();
        assert_eq!(rgba, [30, 20, 10, 255, 3, 2, 1, 4]);
        let (width, height, back) = png_pixels(&encode_png(2, 1, &rgba).unwrap());
        assert_eq!((width, height), (2, 1));
        assert_eq!(back, rgba);
    }

    #[test]
    fn a_dxt1_block_of_one_color_decodes_to_it() {
        // Both endpoints pure red in RGB565, every index 0.
        let block = [0x00, 0xF8, 0x00, 0xF8, 0, 0, 0, 0];
        let format = PixelFormat::of("PF_DXT1").unwrap();
        assert_eq!(format.mip_size(4, 4), 8);
        let rgba = format.decode(4, 4, &block).unwrap();
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 0, 0, 255])
        );
        assert!(format.decode(4, 4, &block[..4]).is_err(), "a short block");
    }

    #[test]
    fn a_size_that_is_not_whole_blocks_decodes_cropped() {
        let block = [0x00, 0xF8, 0x00, 0xF8, 0, 0, 0, 0];
        let format = PixelFormat::of("PF_DXT1").unwrap();
        // 6x2 takes two blocks across and one down.
        let blocks = [block, block].concat();
        let rgba = format.decode(6, 2, &blocks).unwrap();
        assert_eq!(rgba.len(), 6 * 2 * 4);
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 0, 0, 255])
        );
        let bc7 = PixelFormat::of("PF_BC7").unwrap();
        let blocks = vec![0u8; bc7.mip_size(62, 62)];
        assert_eq!(bc7.decode(62, 62, &blocks).unwrap().len(), 62 * 62 * 4);
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let error: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        if error == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0f64 * 255.0 / error).log10()
        }
    }

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    [
                        (x * 255 / width.max(2)) as u8,
                        (y * 255 / height.max(2)) as u8,
                        128,
                        255,
                    ]
                })
            })
            .collect()
    }

    #[test]
    fn eight_bit_pixels_encode_back_exactly() {
        let rgba = gradient(5, 3);
        for name in ["PF_B8G8R8A8", "PF_R8G8B8A8"] {
            let format = PixelFormat::of(name).unwrap();
            let bytes = format.encode(5, 3, &rgba, Quality::Default);
            assert_eq!(format.decode(5, 3, &bytes).unwrap(), rgba, "{name}");
        }
        let grey = PixelFormat::Grey8.encode(1, 1, &[200, 200, 200, 255], Quality::Default);
        assert_eq!(grey, [200]);
    }

    #[test]
    fn block_formats_encode_close_to_the_pixels_at_any_size() {
        for name in ["PF_DXT1", "PF_DXT5", "PF_BC7"] {
            let format = PixelFormat::of(name).unwrap();
            for (width, height) in [(64, 32), (62, 30), (1, 1)] {
                let rgba = gradient(width, height);
                let bytes = format.encode(width, height, &rgba, Quality::Fast);
                assert_eq!(
                    bytes.len(),
                    format.mip_size(width, height),
                    "{name} {width}x{height}"
                );
                let back = format.decode(width, height, &bytes).unwrap();
                assert!(psnr(&rgba, &back) > 30.0, "{name} {width}x{height}");
            }
        }
    }

    #[test]
    fn a_mip_is_half_the_one_above_each_pixel_the_average_it_covers() {
        let rgba = [
            0, 0, 0, 255, 100, 100, 100, 255, 200, 200, 200, 255, 100, 100, 100, 255,
        ];
        let (width, height, below) = half(2, 2, &rgba);
        assert_eq!((width, height), (1, 1));
        assert_eq!(below, [100, 100, 100, 255]);
        // An odd edge repeats its last pixel rather than reading past it.
        let (width, height, _) = half(3, 1, &[0; 12]);
        assert_eq!((width, height), (1, 1));
    }

    #[test]
    fn padding_repeats_the_last_column_and_row() {
        let rgba = [1, 1, 1, 1, 2, 2, 2, 2];
        let (width, height, padded) = pad_to_blocks(2, 1, &rgba);
        assert_eq!((width, height), (4, 4));
        assert_eq!(
            &padded[..16],
            &[1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]
        );
        assert_eq!(&padded[48..], &padded[..16], "the last row repeats");
    }

    /// A texture of this size whose cook left out its `first` largest mips, storing `mips` below.
    fn platform(x: u32, y: u32, first: i32, mips: usize) -> TexturePlatform {
        TexturePlatform {
            format: "PF_DXT1".into(),
            size_x: x,
            size_y: y,
            slices: 1,
            cubemap: false,
            opt_data: None,
            first_mip: first,
            mips: (0..mips)
                .map(|at| {
                    let shift = first as u32 + at as u32;
                    TextureMip {
                        size_x: (x >> shift).max(1),
                        size_y: (y >> shift).max(1),
                        size_z: 1,
                        resource: 0,
                        placement: "inline",
                        size: 0,
                        inline_at: None,
                        offset: 0,
                    }
                })
                .collect(),
            is_virtual: false,
            format_name: Default::default(),
            reserved: [0; 16],
            span: (0, 0),
        }
    }

    #[test]
    fn a_new_size_keeps_the_mip_chain_the_texture_had() {
        assert_eq!(
            mip_count(&platform(128, 128, 0, 8), false, (256, 256, 1)),
            9,
            "a full chain"
        );
        assert_eq!(
            mip_count(&platform(128, 128, 0, 6), false, (256, 256, 1)),
            7,
            "two short"
        );
        assert_eq!(
            mip_count(&platform(1426, 321, 0, 1), false, (800, 180, 1)),
            1,
            "one mip"
        );
        assert_eq!(mip_count(&platform(128, 128, 0, 8), false, (1, 1, 1)), 1);
        // Stored from 128 down to 1, which is a full chain from where it starts.
        assert_eq!(
            mip_count(&platform(256, 256, 1, 8), false, (128, 128, 1)),
            8,
            "the largest left out"
        );
        assert_eq!(
            mip_count(&platform(256, 256, 1, 8), false, (256, 256, 1)),
            9
        );
        // A volume's depth counts toward its chain, an array's slices do not.
        let mut deep = platform(16, 16, 0, 7);
        for (at, mip) in deep.mips.iter_mut().enumerate() {
            mip.size_z = (64 >> at).max(1);
        }
        assert_eq!(mip_count(&deep, true, (16, 16, 64)), 7);
        assert_eq!(mip_count(&deep, true, (32, 32, 64)), 7);
        assert_eq!(mip_count(&deep, true, (32, 32, 128)), 8);
        let mut array = platform(16, 16, 0, 5);
        for mip in &mut array.mips {
            mip.size_z = 64;
        }
        assert_eq!(mip_count(&array, false, (32, 32, 64)), 6);
    }

    /// A volume's next mip averages two by two by two, so its depth halves; a cube's or an array's
    /// layers halve each on its own.
    #[test]
    fn a_volume_halves_in_depth_and_other_layers_on_their_own() {
        let layer = |value: u8| vec![value; 2 * 2 * 4];
        let level = Level {
            width: 2,
            height: 2,
            layers: vec![layer(0), layer(100), layer(200), layer(201), layer(7)],
        };
        let volume = level.half(true);
        assert_eq!((volume.width, volume.height), (1, 1));
        assert_eq!(volume.layers, vec![vec![50; 4], vec![201; 4]]);
        let flat = level.half(false);
        assert_eq!(flat.layers.len(), 5);
        assert_eq!(flat.layers[1], vec![100; 4]);
        let last = Level {
            width: 1,
            height: 1,
            layers: vec![vec![9; 4]],
        };
        assert_eq!(last.half(true).layers, vec![vec![9; 4]]);
    }

    /// The size a texture gives itself follows its largest stored mip, as many times over as the
    /// cook left mips out, and stays as it was for a mip the size it stores now.
    #[test]
    fn a_texture_keeps_its_full_size_above_the_mips_it_stores() {
        let plain = platform(128, 64, 0, 8);
        assert_eq!(plain.full_size((128, 64)), (128, 64));
        assert_eq!(plain.full_size((256, 128)), (256, 128));
        let left_out = platform(1000, 500, 2, 6);
        assert_eq!(left_out.mips[0].size_x, 250);
        assert_eq!(
            left_out.full_size((250, 125)),
            (1000, 500),
            "the size it had"
        );
        assert_eq!(left_out.full_size((512, 256)), (2048, 1024));
    }

    #[test]
    fn a_png_of_any_color_type_reads_as_rgba() {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 2, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[10, 20]).unwrap();
        }
        let (width, height, rgba) = decode_png(&out).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgba, [10, 10, 10, 255, 20, 20, 20, 255]);
    }

    #[test]
    fn a_format_that_cannot_be_decoded_says_why() {
        assert!(PixelFormat::of("PF_FloatRGBA").unwrap().is_hdr());
        assert!(!PixelFormat::of("PF_BC7").unwrap().is_hdr());
        assert!(PixelFormat::of("PF_ASTC_4x4").unwrap_err().contains("BC6H"));
        assert_eq!(PixelFormat::Grey8.mip_size(3, 2), 6);
        assert_eq!(PixelFormat::Half(4).mip_size(3, 2), 48);
        assert_eq!(PixelFormat::Float(1).mip_size(3, 2), 24);
    }

    /// Halves and floats encode back exactly, a one-channel format reading as grey and taking red.
    #[test]
    fn half_and_float_pixels_encode_back_exactly() {
        let rgba = [0.5f32, -2.0, 1024.0, 1.0, 3.25, 0.0, 0.125, 0.75];
        for format in [PixelFormat::Half(4), PixelFormat::Float(4)] {
            let bytes = format.encode_hdr(2, 1, &rgba, Quality::Fast);
            assert_eq!(bytes.len(), format.mip_size(2, 1));
            assert_eq!(format.decode_hdr(2, 1, &bytes).unwrap(), rgba);
        }
        let red = PixelFormat::Half(1).encode_hdr(2, 1, &rgba, Quality::Fast);
        assert_eq!(
            PixelFormat::Half(1).decode_hdr(2, 1, &red).unwrap(),
            [0.5, 0.5, 0.5, 1.0, 3.25, 3.25, 3.25, 1.0]
        );
        assert!(PixelFormat::Half(4).decode(2, 1, &[0; 16]).is_err());
    }

    /// BC6H holds a smooth gradient of light within a few percent, at a size that is not whole
    /// blocks, and clamps what it cannot hold.
    #[test]
    fn bc6h_holds_high_dynamic_range_close() {
        let format = PixelFormat::of("PF_BC6H").unwrap();
        let (width, height) = (10u32, 6u32);
        let rgba: Vec<f32> = (0..width * height)
            .flat_map(|i| {
                let v = 1.5 + (i % width) as f32 * 0.05 + (i / width) as f32 * 0.08;
                [v, v * 0.5 + 0.1, 12.0 - v, 1.0]
            })
            .collect();
        let bytes = format.encode_hdr(width, height, &rgba, Quality::Default);
        assert_eq!(bytes.len(), format.mip_size(width, height));
        let back = format.decode_hdr(width, height, &bytes).unwrap();
        for (got, wanted) in back.iter().zip(&rgba) {
            assert!(
                (got - wanted).abs() <= 0.05 * wanted.abs().max(1.0),
                "{got} for {wanted}"
            );
        }
        let clamped = format.encode_hdr(4, 4, &[-5.0; 64], Quality::Fast);
        assert!(
            format
                .decode_hdr(4, 4, &clamped)
                .unwrap()
                .chunks(4)
                .all(|p| p[0] == 0.0)
        );
    }

    /// An OpenEXR file reads back as written, halves or floats, and alpha reads as 1 where the
    /// file has none.
    #[test]
    fn an_openexr_file_reads_back_as_written() {
        let rgba = [0.5f32, -2.0, 1024.0, 0.25, 3.0, 0.0, 65504.0, 1.0];
        for halves in [true, false] {
            let file = encode_exr(2, 1, &rgba, halves).unwrap();
            assert_eq!(decode_exr(&file).unwrap(), (2, 1, rgba.to_vec()));
        }
        let floats = [1.0e-9f32, 3.0e9, 0.1, 1.0];
        let file = encode_exr(1, 1, &floats, false).unwrap();
        assert_eq!(decode_exr(&file).unwrap().2, floats);
        assert!(decode_exr(b"not an exr").is_err());
    }

    /// High dynamic range mips halve in floats, a volume's depth too.
    #[test]
    fn float_mips_halve_by_their_mean() {
        let level = Level {
            width: 2,
            height: 1,
            layers: vec![
                vec![1.0f32, 2.0, 3.0, 1.0, 3.0, 4.0, 5.0, 1.0],
                vec![5.0f32, 6.0, 7.0, 1.0, 7.0, 8.0, 9.0, 1.0],
            ],
        };
        assert_eq!(level.half(false).layers[0], [2.0, 3.0, 4.0, 1.0]);
        assert_eq!(level.half(true).layers, vec![vec![4.0, 5.0, 6.0, 1.0]]);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use rayon::prelude::*;
    use rivals_uasset::{AssetBundle, encode_texture, is_texture_class, read_header, read_texture};

    use crate::asset::{self, PackageConverter};

    #[derive(Default)]
    struct Probe {
        textures: usize,
        read: usize,
        by_class: BTreeMap<String, usize>,
        formats: BTreeMap<String, usize>,
        placements: BTreeMap<String, usize>,
        words: BTreeMap<Option<u32>, usize>,
        first_mip_set: usize,
        opt_data: usize,
        several_platforms: usize,
        slices: usize,
        virtual_textures: usize,
        /// Textures whose export alone does not read to its end, which a save reads whole.
        read_with_package: usize,
        failures: BTreeMap<String, Vec<String>>,
    }

    fn png_size(path: &std::path::Path) -> (u32, u32) {
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(path).expect("open the png"),
        ));
        let info = decoder.read_info().expect("info");
        (info.info().width, info.info().height)
    }

    /// A mip saves at its own size, a cube's faces one above the other.
    #[test]
    fn a_texture_saves_its_mip_as_a_png_of_that_size() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let dir = std::env::temp_dir().join(format!("rivals-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let request = |entry: &'static str| crate::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &source,
            entry,
            kind: crate::asset::AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };

        let plain = request("Engine/Content/EngineResources/DefaultTexture.uasset");
        let info = super::texture_info(&plain, Some(&schema), None).expect("info");
        let platform = info.layout.platform().expect("one format");
        assert_eq!(platform.format, "PF_DXT1");
        assert_eq!(platform.mips.len(), 8);
        let out = dir.join("plain.png");
        let saved = super::save_image(&plain, Some(&schema), None, Some(1), &out).expect("save");
        assert_eq!((saved.width, saved.height), (64, 64));
        assert_eq!(png_size(&out), (64, 64));

        let cube = request("Engine/Content/EngineResources/DefaultTextureCube.uasset");
        let out = dir.join("cube.png");
        let saved = super::save_image(&cube, Some(&schema), None, None, &out).expect("save");
        assert_eq!((saved.mip, saved.width, saved.height), (0, 128, 128 * 6));
        assert_eq!(png_size(&out), (128, 768));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A texture given a PNG of twice its size takes it whole: a mip chain one longer, the smallest
    /// mips inline as many as before, the rest in its `.ubulk`, its imported size following, and
    /// the package checked as any save is.
    #[test]
    fn a_texture_takes_a_png_at_a_new_size_with_its_mips_rebuilt() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let request = crate::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &source,
            entry: "Engine/Content/EngineResources/DefaultTexture.uasset",
            kind: asset::AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let (loaded, parsed) =
            crate::asset_edit::read_package(&request, Some(&schema)).expect("read");
        let bundle = crate::asset_edit::bundle_of(&loaded);
        let before = read_texture(&bundle, &parsed, 0).expect("layout");
        let kept_inline = before.platforms[0]
            .mips
            .iter()
            .filter(|mip| mip.placement == "inline")
            .count();

        let format = super::PixelFormat::of("PF_DXT1").unwrap();
        let mut levels = vec![(
            256u32,
            256u32,
            (0..256 * 256)
                .flat_map(|i: u32| [(i % 256) as u8, (i / 256) as u8, 64, 255])
                .collect::<Vec<u8>>(),
        )];
        while levels.len() < 9 {
            let (x, y, pixels) = levels.last().unwrap();
            levels.push(super::half(*x, *y, pixels));
        }
        let mips: Vec<rivals_uasset::NewMip> = levels
            .iter()
            .map(|(x, y, pixels)| rivals_uasset::NewMip {
                size_x: *x,
                size_y: *y,
                size_z: 1,
                bytes: format.encode(*x, *y, pixels, super::Quality::Fast),
            })
            .collect();
        let changes = rivals_uasset::PackageEdits {
            textures: vec![rivals_uasset::TextureEdit {
                export: 0,
                mips: mips.clone(),
            }],
            ..Default::default()
        };
        let (patched, after) = crate::asset_edit::patch_pass(
            &request,
            Some(&schema),
            &bundle,
            rivals_uasset::Sidecars {
                bulk: loaded.bulk_data_buffer.as_deref(),
                optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
            },
            &parsed,
            &changes,
        )
        .expect("patch");
        let rebuilt = rivals_uasset::AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let layout = read_texture(&rebuilt, &after, 0).expect("reads back");
        let platform = layout.platform().expect("one format");
        assert_eq!((platform.size_x, platform.size_y), (256, 256));
        assert_eq!(platform.mips.len(), 9);
        let inline = platform
            .mips
            .iter()
            .filter(|m| m.placement == "inline")
            .count();
        assert_eq!(inline, kept_inline, "as many inline as before");
        let ubulk = patched.bulk.as_deref().expect("a new .ubulk");
        let top = &platform.mips[0];
        assert_eq!(top.placement, "ubulk");
        assert_eq!(
            &ubulk[top.offset as usize..][..top.size as usize],
            mips[0].bytes.as_slice()
        );
        let imported = after.exports[0]
            .properties
            .iter()
            .find(|p| p.name == "ImportedSize")
            .expect("imported size");
        let shown = serde_json::to_string(&imported.value).unwrap();
        assert!(shown.contains("256"), "{shown}");
    }

    /// A cube takes its six faces at a new size, each face's mips rebuilt and the faces kept in the
    /// order given; a volume takes its depth at a new width and height, the depth halving down a
    /// chain; an array keeps its slices at every mip and its slice count follows the largest mip.
    /// A dry run takes a stacked PNG and refuses one whose height does not split into the layers.
    #[test]
    fn layered_textures_take_their_layers_stacked() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        const CUBE: &str = "Engine/Content/EngineResources/DefaultTextureCube.uasset";
        const VOLUME: &str =
            "Engine/Content/EngineSky/VolumetricClouds/T_VolumeNoiseShape64.uasset";
        const ARRAY: &str =
            "Engine/Content/EngineMaterials/Substrate/Glints2/AGlintTex2dArray2.uasset";
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let request = |entry: &'static str| crate::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &source,
            entry,
            kind: asset::AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let layout_of = |entry: &'static str| {
            let (loaded, parsed) =
                crate::asset_edit::read_package(&request(entry), Some(&schema)).expect("read");
            read_texture(&crate::asset_edit::bundle_of(&loaded), &parsed, 0).expect("layout")
        };
        // The patched texture's platform data, with its first mip's bytes.
        let patch = |entry: &'static str, mips: Vec<rivals_uasset::NewMip>| {
            let request = request(entry);
            let (loaded, parsed) =
                crate::asset_edit::read_package(&request, Some(&schema)).expect("read");
            let changes = rivals_uasset::PackageEdits {
                textures: vec![rivals_uasset::TextureEdit { export: 0, mips }],
                ..Default::default()
            };
            let (patched, after) = crate::asset_edit::patch_pass(
                &request,
                Some(&schema),
                &crate::asset_edit::bundle_of(&loaded),
                rivals_uasset::Sidecars {
                    bulk: loaded.bulk_data_buffer.as_deref(),
                    optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
                },
                &parsed,
                &changes,
            )?;
            let rebuilt = AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            };
            let layout = read_texture(&rebuilt, &after, 0).expect("reads back");
            let platform = layout.platform().expect("one format").clone();
            let top = &platform.mips[0];
            let at = top.inline_at.expect("inline");
            let bytes = patched.exports[at..at + top.size as usize].to_vec();
            Ok::<_, String>((platform, bytes))
        };

        // Six faces of colors a DXT1 block holds exactly, so each decodes back as given.
        let cube = layout_of(CUBE);
        assert_eq!(rivals_uasset::texture_lock(&cube), None);
        let colors: [[u8; 4]; 6] = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
            [0, 255, 255, 255],
            [255, 0, 255, 255],
        ];
        let faces: Vec<u8> = colors
            .iter()
            .flat_map(|color| color.repeat(64 * 64))
            .collect();
        let format = super::PixelFormat::of(&cube.platform().unwrap().format).unwrap();
        let mips =
            super::build_mips(&cube, format, 64, 64 * 6, &faces, super::Quality::Fast).unwrap();
        assert_eq!(mips.len(), cube.platform().unwrap().mips.len() - 1);
        let (platform, top) = patch(CUBE, mips).expect("patch the cube");
        assert_eq!((platform.size_x, platform.size_y), (64, 64));
        assert_eq!((platform.slices, platform.cubemap), (6, true));
        assert!(platform.mips.iter().all(|mip| mip.size_z == 1));
        let (width, height, rgba) =
            super::decode_mip::<u8>(format, &platform, &platform.mips[0], &top).unwrap();
        assert_eq!((width, height), (64, 64 * 6));
        for (face, color) in colors.iter().enumerate() {
            let at = (face * 64 * 64 + 32 * 64 + 32) * 4;
            assert_eq!(&rgba[at..at + 4], color, "face {face}");
        }

        // A volume halves in depth down a chain, and its slice count follows its largest mip.
        let volume = layout_of(VOLUME);
        assert_eq!(rivals_uasset::texture_lock(&volume), None);
        let format = super::PixelFormat::of(&volume.platform().unwrap().format).unwrap();
        let mip = |x: u32, z: u32| rivals_uasset::NewMip {
            size_x: x,
            size_y: x,
            size_z: z,
            bytes: vec![0; format.mip_size(x, x) * z as usize],
        };
        let (platform, _) =
            patch(VOLUME, vec![mip(32, 64), mip(16, 32), mip(8, 16)]).expect("patch the volume");
        let shape: Vec<(u32, u32)> = platform
            .mips
            .iter()
            .map(|mip| (mip.size_x, mip.size_z))
            .collect();
        assert_eq!(shape, [(32, 64), (16, 32), (8, 16)]);
        assert_eq!(platform.slices, 64);
        let (platform, _) = patch(VOLUME, vec![mip(64, 32)]).expect("a shallower volume");
        assert_eq!(platform.slices, 32);
        let refused = patch(VOLUME, vec![mip(32, 64), mip(16, 64)]).unwrap_err();
        assert!(refused.contains("not a 16x16x32 one"), "{refused}");

        // An array keeps its slices at every mip.
        let array = layout_of(ARRAY);
        assert_eq!(rivals_uasset::texture_lock(&array), None);
        let slices = array.platform().unwrap().slices;
        let mip = |x: u32, z: u32| rivals_uasset::NewMip {
            size_x: x,
            size_y: 1,
            size_z: z,
            bytes: vec![0; x as usize * 8 * z as usize],
        };
        let (platform, _) =
            patch(ARRAY, vec![mip(32, slices), mip(16, slices)]).expect("patch the array");
        assert_eq!(platform.slices, slices);
        assert!(platform.mips.iter().all(|mip| mip.size_z == slices));
        let refused = patch(ARRAY, vec![mip(32, slices), mip(16, slices / 2)]).unwrap_err();
        assert!(refused.contains("not a 16x1x"), "{refused}");

        // A dry run splits the stacked PNG as the save does.
        let dir = std::env::temp_dir().join(format!("rivals-layers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let png = dir.join("faces.png");
        std::fs::write(&png, super::encode_png(64, 64 * 6, &faces).unwrap()).unwrap();
        let dry = |path: &std::path::Path| {
            super::import_image(
                &request(CUBE),
                Some(&schema),
                None,
                path,
                super::Quality::Fast,
                &crate::asset_edit::SaveOptions::default(),
                true,
            )
        };
        let report = dry(&png).expect("dry run");
        assert_eq!((report.to, report.layers), ((64, 64), 6));
        let wrong = dir.join("wrong.png");
        std::fs::write(
            &wrong,
            super::encode_png(64, 64 * 5, &faces[..64 * 64 * 5 * 4]).unwrap(),
        )
        .unwrap();
        let refused = dry(&wrong).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(refused.contains("6 faces stacked"), "{refused}");
    }

    /// A texture whose cook left out its largest mip takes a PNG as the largest mip it stores. At
    /// that size it keeps its own size, its imported size, its first mip and its chain; at twice
    /// that size its own size doubles with them. A dry run says which mip the PNG stands for.
    #[test]
    fn a_texture_with_its_largest_mips_left_out_takes_a_png_as_its_largest_stored_one() {
        const ENTRY: &str = "Marvel/Content/Marvel/Characters/1011/1011001/Slots/Accessories/3810110001/Textures/T_Slot_381011_0001_N.uasset";
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let request = crate::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &source,
            entry: ENTRY,
            kind: asset::AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let (loaded, parsed) =
            crate::asset_edit::read_package(&request, Some(&schema)).expect("read");
        let bundle = crate::asset_edit::bundle_of(&loaded);
        let export = parsed
            .exports
            .iter()
            .find(|export| is_texture_class(&export.class_name))
            .expect("a texture")
            .index;
        let before = read_texture(&bundle, &parsed, export).expect("layout");
        let platform = before.platform().expect("one format");
        assert!(
            platform.first_mip > 0,
            "the fixture's cook left out its largest mip"
        );
        assert_eq!(rivals_uasset::texture_lock(&before), None);
        let format = super::PixelFormat::of(&platform.format).expect("a format PNGs encode to");
        let top = (platform.mips[0].size_x, platform.mips[0].size_y);
        let imported = |parsed: &rivals_uasset::ParsedPackage| {
            parsed.exports[export as usize]
                .properties
                .iter()
                .find(|p| p.name == "ImportedSize")
                .map(|p| serde_json::to_string(&p.value).unwrap())
        };
        let pixels = |x: u32, y: u32| -> Vec<u8> {
            (0..x * y)
                .flat_map(|i| [(i % 256) as u8, (i / 256 % 256) as u8, 128, 255])
                .collect()
        };

        let patch = |scale: u32| {
            let (x, y) = (top.0 * scale, top.1 * scale);
            let mut levels = vec![(x, y, pixels(x, y))];
            while levels.len() < super::mip_count(platform, false, (x, y, 1)) {
                let (x, y, pixels) = levels.last().unwrap();
                levels.push(super::half(*x, *y, pixels));
            }
            let mips: Vec<rivals_uasset::NewMip> = levels
                .par_iter()
                .map(|(x, y, pixels)| rivals_uasset::NewMip {
                    size_x: *x,
                    size_y: *y,
                    size_z: 1,
                    bytes: format.encode(*x, *y, pixels, super::Quality::Fast),
                })
                .collect();
            let changes = rivals_uasset::PackageEdits {
                textures: vec![rivals_uasset::TextureEdit { export, mips }],
                ..Default::default()
            };
            let (patched, after) = crate::asset_edit::patch_pass(
                &request,
                Some(&schema),
                &bundle,
                rivals_uasset::Sidecars {
                    bulk: loaded.bulk_data_buffer.as_deref(),
                    optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
                },
                &parsed,
                &changes,
            )
            .expect("patch");
            let rebuilt = AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            };
            let layout = read_texture(&rebuilt, &after, export).expect("reads back");
            (
                layout.platform().expect("one format").clone(),
                imported(&after),
            )
        };
        let sizes = |platform: &rivals_uasset::TexturePlatform| -> Vec<(u32, u32)> {
            platform.mips.iter().map(|m| (m.size_x, m.size_y)).collect()
        };

        let (same, imported_same) = patch(1);
        assert_eq!(
            (same.size_x, same.size_y),
            (platform.size_x, platform.size_y)
        );
        assert_eq!(same.first_mip, platform.first_mip);
        assert_eq!(sizes(&same), sizes(platform));
        assert_eq!(imported_same, imported(&parsed));

        let (twice, imported_twice) = patch(2);
        assert_eq!(
            (twice.size_x, twice.size_y),
            (platform.size_x * 2, platform.size_y * 2)
        );
        assert_eq!(twice.first_mip, platform.first_mip);
        assert_eq!(twice.mips.len(), platform.mips.len() + 1);
        if let Some(shown) = imported_twice {
            assert!(
                shown.contains(&(platform.size_x * 2).to_string()),
                "{shown}"
            );
        }

        let dir = std::env::temp_dir().join(format!("rivals-first-mip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let png = dir.join("top.png");
        std::fs::write(
            &png,
            super::encode_png(top.0, top.1, &pixels(top.0, top.1)).unwrap(),
        )
        .unwrap();
        let report = super::import_image(
            &request,
            Some(&schema),
            Some(export),
            &png,
            super::Quality::Fast,
            &crate::asset_edit::SaveOptions::default(),
            true,
        )
        .expect("dry run");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(report.from, report.to);
        assert_eq!(report.mips, platform.mips.len());
        let mip = format!("the image is mip {}", platform.first_mip);
        assert!(
            report.warnings.iter().any(|w| w.contains(&mip)),
            "{:?}",
            report.warnings
        );
    }

    /// The platform data writes back as the bytes it was read from, inline mips and all.
    #[test]
    fn a_texture_writes_back_the_bytes_it_was_read_from() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        for entry in [
            "Engine/Content/EngineResources/DefaultTexture.uasset",
            "Engine/Content/EngineResources/DefaultTextureCube.uasset",
            "Engine/Content/EngineResources/DefaultVolumeTexture.uasset",
        ] {
            let loaded =
                asset::load_bundle(&root, &source, entry, asset::AssetSource::Utoc).expect("load");
            let bundle = AssetBundle {
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
            };
            let parsed = rivals_uasset::parse_package(&bundle, Some(&schema)).expect("parse");
            let layout = read_texture(&bundle, &parsed, 0).expect("read");
            let inline: Vec<&[u8]> = layout
                .platforms
                .iter()
                .flat_map(|p| p.mips.iter())
                .filter_map(|mip| {
                    let at = mip.inline_at?;
                    Some(&bundle.exports[at..at + mip.size as usize])
                })
                .collect();
            assert_eq!(
                encode_texture(&layout, &inline).expect("encode"),
                &bundle.exports[layout.span.0..layout.span.1],
                "{entry}"
            );
            assert!(encode_texture(&layout, &[]).is_err() || inline.is_empty());
        }
    }

    /// High dynamic range textures go out as OpenEXR and come back: floats and halves exactly, BC6H
    /// within what it holds, a cube's six faces stacked. A PNG for one, or an OpenEXR file for an
    /// 8-bit texture, is refused naming the kind it takes.
    #[test]
    fn high_dynamic_range_textures_go_out_and_come_back_as_openexr() {
        const FLOATS: &str = "Marvel/Content/Marvel/VFX/VAT/Characters/1032/BAT/SquirrelAssist_Loop/T_SquirrelAssist_Loop_BAT_Pos.uasset";
        const HALVES: &str =
            "Engine/Content/EngineMaterials/Substrate/Glints2/AGlintTex2dArray2.uasset";
        const BC6H: &str = "Marvel/Content/Marvel/Environment/Common/LightingRes/X20_Hdri/DaylightAmbientCubemap_2D.uasset";
        const CUBE: &str =
            "Marvel/Content/Marvel/Environment/Common/LightingRes/X20_Hdri/T_Lighthdr_002.uasset";
        const PLAIN: &str = "Engine/Content/EngineResources/DefaultTexture.uasset";
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let request = |entry: &'static str| crate::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &source,
            entry,
            kind: asset::AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let dir = std::env::temp_dir().join(format!("rivals-exr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let dry = |entry: &'static str, image: &std::path::Path| {
            super::import_image(
                &request(entry),
                Some(&schema),
                None,
                image,
                super::Quality::Fast,
                &crate::asset_edit::SaveOptions::default(),
                true,
            )
        };

        for (entry, exact) in [(FLOATS, true), (HALVES, true), (BC6H, false)] {
            let out = dir.join("out.exr");
            let saved = super::save_image(&request(entry), Some(&schema), None, None, &out)
                .expect("save as OpenEXR");
            let (loaded, parsed) =
                crate::asset_edit::read_package(&request(entry), Some(&schema)).expect("read");
            let layout =
                read_texture(&crate::asset_edit::bundle_of(&loaded), &parsed, 0).expect("layout");
            let platform = layout.platform().expect("one format");
            let format = super::PixelFormat::of(&platform.format).unwrap();
            let original = super::mip_bytes(&loaded, &platform.mips[saved.mip]).unwrap();
            let (width, height, rgba) = super::decode_exr(&std::fs::read(&out).unwrap()).unwrap();
            assert_eq!((width, height), (saved.width, saved.height), "{entry}");
            let mips =
                super::build_mips(&layout, format, width, height, &rgba, super::Quality::Fast)
                    .expect("mips");
            if exact {
                assert!(mips[0].bytes == original, "{entry} comes back as it was");
            } else {
                // BC6H encoded again from its own decoding stays close to it.
                let mip = &platform.mips[saved.mip];
                let was = format.decode_hdr(mip.size_x, mip.size_y, original).unwrap();
                let is = format
                    .decode_hdr(mip.size_x, mip.size_y, &mips[0].bytes)
                    .unwrap();
                let error: f32 = was
                    .iter()
                    .zip(&is)
                    .map(|(a, b)| (a - b).abs() / a.abs().max(0.05))
                    .sum::<f32>()
                    / was.len() as f32;
                assert!(error < 0.02, "{entry}: mean error {error}");
            }
            let report = dry(entry, &out).expect("dry run");
            assert_eq!(report.to, (platform.size_x, platform.size_y), "{entry}");
        }

        // A cube of six faces at another size.
        let faces: Vec<f32> = (0..128 * 128 * 6)
            .flat_map(|i| {
                let v = (i / (128 * 128)) as f32 + 0.5;
                [v, v * 2.0, v * 4.0, 1.0]
            })
            .collect();
        let exr = dir.join("faces.exr");
        std::fs::write(&exr, super::encode_exr(128, 128 * 6, &faces, true).unwrap()).unwrap();
        let report = dry(CUBE, &exr).expect("a cube's faces");
        assert_eq!((report.to, report.layers, report.mips), ((128, 128), 6, 8));

        // Each kind of texture refuses the other kind of image.
        let refused = super::save_image(
            &request(FLOATS),
            Some(&schema),
            None,
            None,
            &dir.join("out.png"),
        )
        .unwrap_err();
        assert!(refused.contains("OpenEXR (.exr)"), "{refused}");
        let png = dir.join("plain.png");
        std::fs::write(&png, super::encode_png(1, 1, &[1, 2, 3, 255]).unwrap()).unwrap();
        let refused = dry(FLOATS, &png).unwrap_err();
        assert!(refused.contains("OpenEXR (.exr)"), "{refused}");
        let refused = dry(PLAIN, &exr).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(refused.contains("a PNG"), "{refused}");
    }

    /// A texture sharing its package's bulk data with another texture leaves the other's entries
    /// and bytes as they were. Growing, it takes an entry added at the end of the table; shrinking,
    /// it leaves its spare entries empty, or drops them where they end the table.
    #[test]
    fn a_texture_beside_other_bulk_data_leaves_it_as_it_was() {
        const ENTRY: &str = "Marvel/Content/Marvel/Characters/1030/1030302/Weapons/MVP/Chain/Textures/T_WP_1030302_Chain_MRO.uasset";
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let request = crate::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &source,
            entry: ENTRY,
            kind: asset::AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let (loaded, parsed) =
            crate::asset_edit::read_package(&request, Some(&schema)).expect("read");
        let bundle = crate::asset_edit::bundle_of(&loaded);
        let table = read_header(&bundle).expect("header").data_resources;
        assert_eq!(table.len(), 22, "two textures of 11 mips");
        // Each mip's size and bytes, from the export data or the .ubulk.
        let mips = |bundle: &AssetBundle<'_>,
                    parsed: &rivals_uasset::ParsedPackage,
                    ubulk: Option<&[u8]>,
                    export: u32| {
            let layout = read_texture(bundle, parsed, export).expect("layout");
            layout
                .platform()
                .expect("one format")
                .mips
                .iter()
                .map(|mip| {
                    let size = mip.size as usize;
                    let bytes = match mip.inline_at {
                        Some(at) => bundle.exports[at..at + size].to_vec(),
                        None => ubulk.expect("a .ubulk")[mip.offset as usize..][..size].to_vec(),
                    };
                    (mip.size_x, mip.size_y, bytes)
                })
                .collect::<Vec<_>>()
        };
        let before = |export| mips(&bundle, &parsed, loaded.bulk_data_buffer.as_deref(), export);
        let format = super::PixelFormat::of("PF_DXT1").unwrap();
        // Gives `export` a chain from `size` down of bytes nothing else holds; returns the table
        // and both textures' mips after.
        let patch = |export: u32, size: u32| {
            let mut chain = Vec::new();
            let mut x = size;
            loop {
                let level = chain.len() as u8;
                chain.push(rivals_uasset::NewMip {
                    size_x: x,
                    size_y: x,
                    size_z: 1,
                    bytes: (0..format.mip_size(x, x))
                        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(level))
                        .collect(),
                });
                if x == 1 {
                    break;
                }
                x /= 2;
            }
            let changes = rivals_uasset::PackageEdits {
                textures: vec![rivals_uasset::TextureEdit {
                    export,
                    mips: chain.clone(),
                }],
                ..Default::default()
            };
            let (patched, after) = crate::asset_edit::patch_pass(
                &request,
                Some(&schema),
                &bundle,
                rivals_uasset::Sidecars {
                    bulk: loaded.bulk_data_buffer.as_deref(),
                    optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
                },
                &parsed,
                &changes,
            )
            .expect("patch");
            let rebuilt = AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            };
            let table = read_header(&rebuilt).expect("header").data_resources;
            let ubulk = patched.bulk.as_deref();
            let given: Vec<_> = chain
                .into_iter()
                .map(|mip| (mip.size_x, mip.size_y, mip.bytes))
                .collect();
            assert_eq!(mips(&rebuilt, &after, ubulk, export), given);
            (table, mips(&rebuilt, &after, ubulk, 1 - export))
        };

        // Growing, it keeps its entries and takes one more at the end.
        let (grown, other) = patch(0, 2048);
        assert_eq!(grown.len(), 23);
        assert_eq!(
            other,
            before(1),
            "the other texture's mips are as they were"
        );
        assert_eq!(
            &grown[11..22]
                .iter()
                .map(|r| r.serial_size)
                .collect::<Vec<_>>(),
            &table[11..22]
                .iter()
                .map(|r| r.serial_size)
                .collect::<Vec<_>>()
        );

        // Shrinking, its spare entries are left empty in the .ubulk, where nothing reads them.
        let (shrunk, other) = patch(0, 256);
        assert_eq!(shrunk.len(), 22);
        assert_eq!(other, before(1));
        for spare in [9, 10] {
            assert_eq!(shrunk[spare].serial_size, 0, "entry {spare}");
            assert_eq!(
                rivals_uasset::placement(shrunk[spare].legacy_bulk_data_flags),
                "ubulk"
            );
        }

        // The last texture's spare entries end the table, so they go.
        let (trimmed, other) = patch(1, 256);
        assert_eq!(trimmed.len(), 20);
        assert_eq!(other, before(0));
    }

    /// The textures the fast save is compared on: inline mips, `.ubulk` ones, a cube, a volume, an
    /// array, HDR ones, and weightmaps and a heightmap in maps of thousands of exports.
    const ALONE_CASES: &[(&str, Option<&str>)] = &[
        ("Engine/Content/EngineResources/DefaultTexture.uasset", None),
        (
            "Marvel/Content/Marvel/AbilitySystem/Cue/T_SprayPaint_Default.uasset",
            None,
        ),
        (
            "Engine/Content/EngineResources/DefaultTextureCube.uasset",
            None,
        ),
        (
            "Engine/Content/EngineSky/VolumetricClouds/T_VolumeNoiseShape64.uasset",
            None,
        ),
        (
            "Engine/Content/EngineMaterials/Substrate/Glints2/AGlintTex2dArray2.uasset",
            None,
        ),
        (
            "Marvel/Content/Marvel/VFX/VAT/Characters/1032/BAT/SquirrelAssist_Loop/T_SquirrelAssist_Loop_BAT_Pos.uasset",
            None,
        ),
        (
            "Marvel/Content/Marvel/Maps/KrakoaC/KrakoaC01/KrakoaC01_Art.umap",
            Some("KrakoaC01_Art:PersistentLevel:Landscape_1:Weightmap_120"),
        ),
        (
            "Marvel/Content/Marvel/Maps/KrakoaC/KrakoaC01/KrakoaC01_Art.umap",
            Some("Heightmap_0"),
        ),
        (
            "Marvel/Content/Marvel/Maps/KunlunH01/KunlunH01_Art.umap",
            Some("KunlunH01_Art:PersistentLevel:Landscape_0:Weightmap_11"),
        ),
    ];

    /// What a game texture's save is compared on: where to read it, the texture, and new mips for
    /// it at `scale` times its size, a gradient no texture holds.
    struct AloneCase {
        root: String,
        source: String,
        entry: &'static str,
        schema: std::sync::Arc<rivals_uasset::Mappings>,
        texture: rivals_uasset::ParsedExport,
        changes: rivals_uasset::PackageEdits,
    }

    impl AloneCase {
        fn open(entry: &'static str, export: Option<&str>, scale: u32) -> Option<Self> {
            let (Ok(root), Ok(usmap)) = (
                std::env::var("RIVALS_GAME_ROOT"),
                std::env::var("RIVALS_USMAP"),
            ) else {
                return None;
            };
            std::env::var_os("OODLE_LIB_PATH")?;
            let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
            let source = format!(
                "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
                root.replace('\\', "/")
            );
            let request = crate::asset_edit::AssetEditRequest {
                game_root: &root,
                container: &source,
                entry,
                kind: asset::AssetSource::Utoc,
                mod_name: "",
                changes: Default::default(),
            };
            let loaded =
                asset::load_bundle(&root, &source, entry, asset::AssetSource::Utoc).expect("load");
            let bundle = crate::asset_edit::bundle_of(&loaded);
            let rows = rivals_uasset::export_rows(&bundle).expect("rows");
            let index = export.map(|name| {
                rivals_uasset::export_row_of(&rows, name)
                    .unwrap_or_else(|e| panic!("{entry}: {e}"))
                    .index
            });
            let (_, row, layout) =
                super::read_layout(&request, Some(&schema), index).expect("layout");
            let platform = layout.platform().expect("one format");
            let format = super::PixelFormat::of(&platform.format).expect("format");
            let (width, height) = (
                platform.mips[0].size_x * scale,
                platform.mips[0].size_y * scale,
            );
            let stacked = height * super::layer_count(&layout, platform);
            let at = |i: u32| ((i % width) * 255 / width.max(1), (i / width) % 256);
            let mips = if format.is_hdr() {
                let pixels: Vec<f32> = (0..width * stacked)
                    .flat_map(|i| {
                        let (x, y) = at(i);
                        [x as f32 / 64.0, y as f32 / 64.0, 0.5, 1.0]
                    })
                    .collect();
                super::build_mips(
                    &layout,
                    format,
                    width,
                    stacked,
                    &pixels,
                    super::Quality::Fast,
                )
            } else {
                let pixels: Vec<u8> = (0..width * stacked)
                    .flat_map(|i| {
                        let (x, y) = at(i);
                        [x as u8, y as u8, 96, 255]
                    })
                    .collect();
                super::build_mips(
                    &layout,
                    format,
                    width,
                    stacked,
                    &pixels,
                    super::Quality::Fast,
                )
            }
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
            let texture = rivals_uasset::parse_exports_at(
                &bundle,
                Some(&schema),
                None,
                rivals_uasset::ParseOptions::default(),
                &[layout.export],
            )
            .expect("read alone")
            .pop()
            .expect("the texture");
            let changes = rivals_uasset::PackageEdits {
                textures: vec![rivals_uasset::TextureEdit {
                    export: layout.export,
                    mips,
                }],
                expect: rivals_uasset::Expected {
                    exports: [(layout.export, row.path)].into(),
                    ..Default::default()
                },
                ..Default::default()
            };
            Some(Self {
                root,
                source,
                entry,
                schema,
                texture,
                changes,
            })
        }

        fn request(&self) -> crate::asset_edit::AssetEditRequest<'_> {
            crate::asset_edit::AssetEditRequest {
                game_root: &self.root,
                container: &self.source,
                entry: self.entry,
                kind: asset::AssetSource::Utoc,
                mod_name: "unused, nothing is written",
                changes: self.changes.clone(),
            }
        }
    }

    /// A save of new pixels and nothing else reads only the texture, is held to the package's
    /// bytes, and saves exactly what reading the whole package before and after saves, at the
    /// texture's size and at twice it.
    #[test]
    fn a_texture_saved_from_its_export_alone_saves_what_a_whole_read_does() {
        for &(entry, export) in ALONE_CASES {
            for scale in [1, 2] {
                let Some(case) = AloneCase::open(entry, export, scale) else {
                    return;
                };
                let request = case.request();
                let loaded =
                    asset::load_bundle(&case.root, &case.source, entry, asset::AssetSource::Utoc)
                        .expect("load");
                let alone = rivals_uasset::patch_textures_alone(
                    &crate::asset_edit::bundle_of(&loaded),
                    rivals_uasset::Sidecars {
                        bulk: loaded.bulk_data_buffer.as_deref(),
                        optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
                    },
                    Some(&case.schema),
                    &case.changes,
                )
                .unwrap_or_else(|e| panic!("{entry} at {scale}x: {e}"));
                assert!(alone.is_some(), "{entry} at {scale}x reads whole by itself");
                let options = crate::asset_edit::SaveOptions::default();
                let fast = crate::asset_edit::prepared(&request, Some(&case.schema), &options)
                    .unwrap_or_else(|e| panic!("{entry} at {scale}x: {e}"));
                let whole = crate::asset_edit::whole_read(|| {
                    crate::asset_edit::prepared(&request, Some(&case.schema), &options)
                })
                .unwrap_or_else(|e| panic!("{entry} at {scale}x, read whole: {e}"));
                let what = format!("{entry} at {scale}x");
                assert!(fast.asset == whole.asset, "{what}: the header");
                assert!(fast.exports == whole.exports, "{what}: the exports");
                assert!(fast.bulk == whole.bulk, "{what}: the .ubulk");
                assert!(
                    fast.optional_bulk == whole.optional_bulk,
                    "{what}: the .uptnl"
                );
                assert_eq!(fast.notes, whole.notes, "{what}");
                assert_eq!(
                    format!("{:?}", fast.applied),
                    format!("{:?}", whole.applied),
                    "{what}"
                );
            }
        }
    }

    /// A texture save held to the package's bytes refuses anything it did not make: a name, an
    /// import, a preload dependency, a flag on the texture's export, another payload's entry, a
    /// byte of another export or of what follows the last, the texture's imported size, and a
    /// `.ubulk` longer than its entries.
    #[test]
    fn a_texture_save_is_refused_anything_outside_its_pixels() {
        use rivals_uasset::PatchedBundle;
        for (entry, export) in [ALONE_CASES[0], ALONE_CASES[6]] {
            let Some(case) = AloneCase::open(entry, export, 2) else {
                return;
            };
            let loaded =
                asset::load_bundle(&case.root, &case.source, entry, asset::AssetSource::Utoc)
                    .expect("load");
            let bundle = crate::asset_edit::bundle_of(&loaded);
            let sidecars = rivals_uasset::Sidecars {
                bulk: loaded.bulk_data_buffer.as_deref(),
                optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
            };
            let patched = rivals_uasset::patch_textures_alone(
                &bundle,
                sidecars,
                Some(&case.schema),
                &case.changes,
            )
            .expect("patched")
            .expect("whole by itself");
            let verify = |after: &PatchedBundle| {
                rivals_uasset::verify_texture_save(
                    &bundle,
                    sidecars,
                    after,
                    std::slice::from_ref(&case.texture),
                    Some(&case.schema),
                )
            };
            verify(&patched).expect("the save as made");
            let copy = || PatchedBundle {
                asset: patched.asset.clone(),
                exports: patched.exports.clone(),
                applied: Vec::new(),
                bulk: patched.bulk.clone(),
                optional_bulk: patched.optional_bulk.clone(),
                notes: Vec::new(),
            };
            let header = rivals_uasset::read_header(&rivals_uasset::AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            })
            .expect("header");
            let total = header.summary.versioning_info.total_header_size as i64;
            // The header written again with one change, as a save writes it.
            let reheader = |change: &dyn Fn(&mut retoc::legacy_asset::FLegacyPackageHeader)| {
                let mut changed = header.clone();
                for export in &mut changed.exports {
                    export.serial_offset -= total;
                }
                change(&mut changed);
                let mut out = std::io::Cursor::new(Vec::new());
                changed
                    .serialize(
                        &mut out,
                        Some(total as usize),
                        &retoc::logging::Log::no_log(),
                    )
                    .expect("serialize");
                let mut after = copy();
                after.asset = out.into_inner();
                after
            };
            assert!(
                reheader(&|_| {}).asset == patched.asset,
                "{entry}: the header writes back"
            );
            let at = case.texture.index as usize;
            let own: Vec<i32> = rivals_uasset::read_texture_export(&bundle, &case.texture)
                .expect("layout")
                .platforms[0]
                .mips
                .iter()
                .map(|mip| mip.resource)
                .collect();
            let other =
                (0..header.exports.len()).find(|&i| i != at && header.exports[i].serial_size > 8);
            let mut tampered: Vec<(&str, PatchedBundle)> = vec![
                (
                    "an import",
                    reheader(&|h| h.imports[0].is_optional = !h.imports[0].is_optional),
                ),
                (
                    "the texture's flags",
                    reheader(&|h| h.exports[at].object_flags ^= 1),
                ),
            ];
            let mut name = copy();
            let script = name
                .asset
                .windows(8)
                .position(|w| w == b"/Script/")
                .expect("a script name");
            name.asset[script + 8] ^= 0x20;
            tampered.push(("a name", name));
            if !header.preload_dependencies.is_empty() {
                tampered.push((
                    "a preload dependency",
                    reheader(&|h| h.preload_dependencies[0].index ^= 1),
                ));
            }
            // An entry the package had for another export's payload; the ones past it are the
            // texture's new mips'.
            let had = rivals_uasset::read_header(&bundle)
                .expect("header")
                .data_resources
                .len();
            if let Some(resource) = (0..had)
                .find(|&r| !own.contains(&(r as i32)) && header.data_resources[r].serial_size > 0)
            {
                tampered.push((
                    "another payload's entry",
                    reheader(&|h| h.data_resources[resource].raw_size += 1),
                ));
            }
            if let Some(other) = other {
                let mut byte = copy();
                let place = (header.exports[other].serial_offset - total) as usize + 4;
                byte.exports[place] ^= 1;
                tampered.push(("another export's byte", byte));
            }
            let mut tail = copy();
            *tail.exports.last_mut().expect("bytes") ^= 1;
            tampered.push(("what follows the last export", tail));
            if let Some((first, _)) = case
                .texture
                .properties
                .iter()
                .find(|p| p.name == "ImportedSize")
                .and_then(|p| p.span)
            {
                // Its place in the export data, which the texture's own start keeps.
                let total_before = rivals_uasset::read_header(&bundle)
                    .expect("header")
                    .summary
                    .versioning_info
                    .total_header_size as i64;
                let mut imported = copy();
                imported.exports[(first as i64 - total_before) as usize] ^= 1;
                tampered.push(("the imported size", imported));
            }
            if patched.bulk.is_some() {
                let mut ubulk = copy();
                ubulk.bulk.as_mut().expect("a .ubulk").push(0);
                tampered.push(("a .ubulk longer than its entries", ubulk));
            }
            assert!(tampered.len() >= 6, "{entry}: {} cases", tampered.len());
            for (what, after) in &tampered {
                assert!(verify(after).is_err(), "{entry}: {what} went unnoticed");
            }
        }
    }

    /// Textures of one package given new pixels in one save: what saving them one after another
    /// writes, whether made from the textures' exports alone or from reads of the whole package.
    /// One given twice is refused.
    #[test]
    fn several_textures_in_one_save_write_what_saving_them_in_turn_does() {
        let maps: [(&str, &[&str]); 2] = [
            (
                "Marvel/Content/Marvel/Maps/KrakoaC/KrakoaC01/KrakoaC01_Art.umap",
                &[
                    "KrakoaC01_Art:PersistentLevel:Landscape_1:Weightmap_120",
                    "Heightmap_0",
                    "KrakoaC01_Art:PersistentLevel:Landscape_1:Weightmap_101",
                ],
            ),
            (
                "Marvel/Content/Marvel/Maps/KunlunH01/KunlunH01_Art.umap",
                &[
                    "KunlunH01_Art:PersistentLevel:Landscape_0:Weightmap_11",
                    "KunlunH01_Art:PersistentLevel:Landscape_0:Weightmap_13",
                ],
            ),
        ];
        for (entry, names) in maps {
            let mut cases = Vec::new();
            for (at, name) in names.iter().enumerate() {
                // The first at twice its size, so the table and the header grow under the rest.
                let Some(case) = AloneCase::open(entry, Some(name), if at == 0 { 2 } else { 1 })
                else {
                    return;
                };
                cases.push(case);
            }
            let mut together = rivals_uasset::PackageEdits::default();
            for case in &cases {
                together.merge(case.changes.clone());
            }
            let first = &cases[0];
            let loaded =
                asset::load_bundle(&first.root, &first.source, entry, asset::AssetSource::Utoc)
                    .expect("load");
            let bundle = crate::asset_edit::bundle_of(&loaded);
            let sidecars = rivals_uasset::Sidecars {
                bulk: loaded.bulk_data_buffer.as_deref(),
                optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
            };
            let batch = rivals_uasset::patch_textures_alone(
                &bundle,
                sidecars,
                Some(&first.schema),
                &together,
            )
            .unwrap_or_else(|e| panic!("{entry}: {e}"))
            .expect("whole by themselves");

            // One save after another, each reading what the one before it wrote.
            let (mut asset, mut exports) = (
                loaded.asset_file_buffer.clone(),
                loaded.exports_file_buffer.clone(),
            );
            let mut bulk = loaded.bulk_data_buffer.clone();
            for case in &cases {
                let step = rivals_uasset::patch_textures_alone(
                    &rivals_uasset::AssetBundle {
                        asset: &asset,
                        exports: &exports,
                    },
                    rivals_uasset::Sidecars {
                        bulk: bulk.as_deref(),
                        optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
                    },
                    Some(&case.schema),
                    &rivals_uasset::PackageEdits {
                        textures: case.changes.textures.clone(),
                        ..Default::default()
                    },
                )
                .unwrap_or_else(|e| panic!("{entry}: {e}"))
                .expect("whole by itself");
                asset = step.asset;
                exports = step.exports;
                if step.bulk.is_some() {
                    bulk = step.bulk;
                }
            }
            assert!(batch.asset == asset, "{entry}: the header");
            assert!(batch.exports == exports, "{entry}: the exports");
            assert!(
                batch.bulk.as_deref().or(loaded.bulk_data_buffer.as_deref()) == bulk.as_deref(),
                "{entry}: the .ubulk"
            );

            let request = crate::asset_edit::AssetEditRequest {
                changes: together.clone(),
                ..first.request()
            };
            let options = crate::asset_edit::SaveOptions::default();
            let fast = crate::asset_edit::prepared(&request, Some(&first.schema), &options)
                .unwrap_or_else(|e| panic!("{entry}: {e}"));
            let whole = crate::asset_edit::whole_read(|| {
                crate::asset_edit::prepared(&request, Some(&first.schema), &options)
            })
            .unwrap_or_else(|e| panic!("{entry}, read whole: {e}"));
            assert!(
                fast.asset == batch.asset && fast.exports == batch.exports,
                "{entry}"
            );
            assert!(fast.asset == whole.asset, "{entry}: the header read whole");
            assert!(
                fast.exports == whole.exports,
                "{entry}: the exports read whole"
            );
            assert!(fast.bulk == whole.bulk, "{entry}: the .ubulk read whole");
            assert_eq!(fast.notes, whole.notes, "{entry}");
            assert_eq!(fast.applied.len(), names.len(), "{entry}");

            let mut twice = together.clone();
            twice.textures.push(cases[0].changes.textures[0].clone());
            let refused =
                rivals_uasset::patch_textures_alone(&bundle, sidecars, Some(&first.schema), &twice)
                    .err()
                    .unwrap_or_default();
            assert!(refused.contains("twice"), "{entry}: {refused}");
        }
    }

    /// An edit file gives textures images as `asset texture --in` does, each named by its path and
    /// encoded against the package as the save reads it, and two edit lists for the same package
    /// merge into one save of both.
    #[test]
    fn an_edit_file_gives_textures_their_images() {
        const MAP: &str = "Marvel/Content/Marvel/Maps/KrakoaC/KrakoaC01/KrakoaC01_Art.umap";
        const WEIGHTMAP: &str = "KrakoaC01_Art:PersistentLevel:Landscape_1:Weightmap_120";
        const HEIGHTMAP: &str = "Heightmap_0";
        let Some(case) = AloneCase::open(MAP, Some(WEIGHTMAP), 1) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("rivals-edit-textures-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let request = crate::asset_edit::AssetEditRequest {
            changes: Default::default(),
            ..case.request()
        };
        // Each texture's own largest mip, as `--out` saves it, a little changed.
        let mut images = Vec::new();
        for (name, file) in [(WEIGHTMAP, "weightmap.png"), (HEIGHTMAP, "heightmap.png")] {
            let loaded =
                asset::load_bundle(&case.root, &case.source, MAP, asset::AssetSource::Utoc)
                    .expect("load");
            let rows =
                rivals_uasset::export_rows(&crate::asset_edit::bundle_of(&loaded)).expect("rows");
            let index = rivals_uasset::export_row_of(&rows, name)
                .expect("named")
                .index;
            let path = dir.join(file);
            super::save_image(&request, Some(&case.schema), Some(index), Some(0), &path)
                .expect("saved");
            let decoder = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(&path).expect("open"),
            ));
            let mut reader = decoder.read_info().expect("info");
            let mut pixels = vec![0; reader.output_buffer_size().expect("size")];
            let info = reader.next_frame(&mut pixels).expect("frame");
            for byte in pixels.iter_mut().step_by(7) {
                *byte = byte.wrapping_add(31);
            }
            let mut encoder = png::Encoder::new(
                std::io::BufWriter::new(std::fs::File::create(&path).expect("create")),
                info.width,
                info.height,
            );
            encoder.set_color(info.color_type);
            encoder.set_depth(info.bit_depth);
            encoder
                .write_header()
                .expect("header")
                .write_image_data(&pixels[..info.buffer_size()])
                .expect("write");
            images.push((index, name, file));
        }
        let json = format!(
            r#"{{"textures": [{{"object": "{}", "image": "{}"}}, {{"object": "{}", "image": "{}", "quality": "fast"}}]}}"#,
            images[0].1, images[0].2, images[1].1, images[1].2
        );
        let list: crate::asset_edit::json::EditList = serde_json::from_str(&json).expect("parse");
        let from_file = list.resolve(&dir).expect("resolve");
        assert_eq!(from_file.texture_images.len(), 2);
        assert!(std::path::Path::new(&from_file.texture_images[0].image).is_absolute());

        let options = crate::asset_edit::SaveOptions::default();
        let saved = |changes: rivals_uasset::PackageEdits| {
            crate::asset_edit::prepared(
                &crate::asset_edit::AssetEditRequest {
                    changes,
                    ..case.request()
                },
                Some(&case.schema),
                &options,
            )
            .expect("saved")
        };
        let filed = saved(from_file.clone());
        let given = saved(rivals_uasset::PackageEdits {
            texture_images: images
                .iter()
                .enumerate()
                .map(|(at, (index, _, file))| rivals_uasset::TextureImage {
                    export: *index,
                    object: None,
                    image: dir.join(file).to_string_lossy().into_owned(),
                    quality: (at == 1).then(|| "fast".to_string()),
                })
                .collect(),
            ..Default::default()
        });
        assert!(
            filed.asset == given.asset && filed.exports == given.exports,
            "by path as by index"
        );
        let mut merged = rivals_uasset::PackageEdits::default();
        for image in &from_file.texture_images {
            merged.merge(rivals_uasset::PackageEdits {
                texture_images: vec![image.clone()],
                ..Default::default()
            });
        }
        let merged = saved(merged);
        assert!(
            merged.asset == filed.asset && merged.exports == filed.exports,
            "merged"
        );
        assert!(merged.bulk == filed.bulk, "merged .ubulk");
        assert_eq!(filed.applied.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every texture the game ships reads its platform data to the end of its export. Slow, so it
    /// runs only when asked for with `RIVALS_TEXTURE_PROBE`.
    #[test]
    fn every_game_texture_reads_its_platform_data() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("RIVALS_TEXTURE_PROBE").is_none() {
            return;
        }
        let schema = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let (store, packages) = asset::list_game_packages(&root).expect("list");
        let probe = Mutex::new(Probe::default());
        packages.par_chunks(256).for_each(|run| {
            let converter = PackageConverter::new(&*store);
            let mut local = Probe::default();
            for (id, path) in run {
                let Ok(bundle) = converter.convert(*id, path) else {
                    continue;
                };
                let bundle = AssetBundle {
                    asset: &bundle.asset_file_buffer,
                    exports: &bundle.exports_file_buffer,
                };
                let Ok(header) = read_header(&bundle) else {
                    continue;
                };
                let class_of = |index: usize| -> Option<String> {
                    let class = header.exports[index].class_index;
                    if !class.is_import() {
                        return None;
                    }
                    let import = header.imports.get(class.to_import_index() as usize)?;
                    header
                        .name_map
                        .get(import.object_name)
                        .ok()
                        .map(|name| name.into_owned())
                };
                let textures: Vec<(usize, String)> = (0..header.exports.len())
                    .filter_map(|index| Some((index, class_of(index)?)))
                    .filter(|(_, class)| is_texture_class(class))
                    .collect();
                if textures.is_empty() {
                    continue;
                }
                let parsed = match rivals_uasset::parse_package(&bundle, Some(&schema)) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        local.textures += textures.len();
                        local
                            .failures
                            .entry(format!("parse: {}", e.chars().take(60).collect::<String>()))
                            .or_default()
                            .push(path.clone());
                        continue;
                    }
                };
                for (index, class) in textures {
                    local.textures += 1;
                    *local.by_class.entry(class.clone()).or_default() += 1;
                    match read_texture(&bundle, &parsed, index as u32) {
                        Ok(layout) => {
                            local.read += 1;
                            let inline: Vec<&[u8]> = layout
                                .platforms
                                .iter()
                                .flat_map(|p| p.mips.iter())
                                .filter_map(|mip| {
                                    let at = mip.inline_at?;
                                    Some(&bundle.exports[at..at + mip.size as usize])
                                })
                                .collect();
                            let span = &bundle.exports[layout.span.0..layout.span.1];
                            if encode_texture(&layout, &inline).as_deref() != Ok(span) {
                                local
                                    .failures
                                    .entry(format!("{class}: does not write back the same"))
                                    .or_default()
                                    .push(path.clone());
                            }
                            // The parse read the same platform data, so the export is whole and
                            // every name in it is on record.
                            let export = &parsed.exports[index];
                            if !matches!(export.status, rivals_uasset::ExportStatus::Complete)
                                || !export.names_complete
                            {
                                local
                                    .failures
                                    .entry(format!("{class}: the parse did not follow it whole"))
                                    .or_default()
                                    .push(path.clone());
                            }
                            // Read from its export alone, as a save of its pixels reads it, the
                            // texture lays out the same.
                            let alone = rivals_uasset::parse_exports_at(
                                &bundle,
                                Some(&schema),
                                None,
                                rivals_uasset::ParseOptions::default(),
                                &[index as u32],
                            );
                            match alone.as_ref().map(|alone| {
                                alone
                                    .first()
                                    .filter(|export| export.properties_end.is_some())
                            }) {
                                Ok(Some(export)) => {
                                    let same = rivals_uasset::read_texture_export(&bundle, export)
                                        .is_ok_and(|read| {
                                            format!("{read:?}") == format!("{layout:?}")
                                        });
                                    if !same {
                                        local
                                            .failures
                                            .entry(format!(
                                                "{class}: read alone, it lays out otherwise"
                                            ))
                                            .or_default()
                                            .push(path.clone());
                                    }
                                }
                                Ok(None) => local.read_with_package += 1,
                                Err(e) => local
                                    .failures
                                    .entry(format!("{class}: read alone: {e}"))
                                    .or_default()
                                    .push(path.clone()),
                            }
                            *local.words.entry(layout.mip_data_word).or_default() += 1;
                            local.several_platforms += usize::from(layout.platforms.len() > 1);
                            for platform in &layout.platforms {
                                *local.formats.entry(platform.format.clone()).or_default() += 1;
                                local.first_mip_set += usize::from(platform.first_mip != 0);
                                local.opt_data += usize::from(platform.opt_data.is_some());
                                local.slices += usize::from(platform.slices != 1);
                                let placements: Vec<&str> =
                                    platform.mips.iter().map(|m| m.placement).collect();
                                let mut kinds = placements.clone();
                                kinds.dedup();
                                *local.placements.entry(kinds.join("+")).or_default() += 1;
                            }
                        }
                        Err(e) => {
                            // Group by the reason without its offsets.
                            let key: String = e
                                .split(" at offset 0x")
                                .next()
                                .unwrap_or(&e)
                                .chars()
                                .take(90)
                                .collect();
                            if key.contains("is virtual") {
                                local.virtual_textures += 1;
                                continue;
                            }
                            local
                                .failures
                                .entry(format!("{class}: {key}"))
                                .or_default()
                                .push(path.clone());
                        }
                    }
                }
            }
            let mut all = probe.lock().unwrap();
            all.textures += local.textures;
            all.read += local.read;
            all.first_mip_set += local.first_mip_set;
            all.opt_data += local.opt_data;
            all.several_platforms += local.several_platforms;
            all.slices += local.slices;
            all.virtual_textures += local.virtual_textures;
            all.read_with_package += local.read_with_package;
            let merge = |map: &mut BTreeMap<String, usize>, add: BTreeMap<String, usize>| {
                for (key, count) in add {
                    *map.entry(key).or_default() += count;
                }
            };
            merge(&mut all.by_class, local.by_class);
            merge(&mut all.formats, local.formats);
            merge(&mut all.placements, local.placements);
            for (key, count) in local.words {
                *all.words.entry(key).or_default() += count;
            }
            for (key, paths) in local.failures {
                all.failures.entry(key).or_default().extend(paths);
            }
        });
        let probe = probe.into_inner().unwrap();
        println!(
            "textures {} read {} virtual {}, read with their package {}",
            probe.textures, probe.read, probe.virtual_textures, probe.read_with_package
        );
        println!("by class {:?}", probe.by_class);
        println!("formats {:?}", probe.formats);
        println!("mip placements {:?}", probe.placements);
        println!("mip data words {:?}", probe.words);
        println!(
            "first mip set {}, opt data {}, several platforms {}, slices != 1 {}",
            probe.first_mip_set, probe.opt_data, probe.several_platforms, probe.slices
        );
        for (reason, paths) in &probe.failures {
            println!(
                "FAIL {} x{}: {:?}",
                reason,
                paths.len(),
                &paths[..paths.len().min(3)]
            );
        }
        assert!(
            probe.failures.is_empty(),
            "every texture but a virtual one reads, alone as with its package, and writes back the same"
        );
    }
}
