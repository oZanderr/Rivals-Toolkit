//! Reads and writes the image files a texture goes out to and comes in from: PNG, OpenEXR, DDS,
//! TGA, BMP and JPEG.

use std::path::Path;

use ddsfile::{
    AlphaMode, Caps2, D3D10ResourceDimension, D3DFormat, Dds, DxgiFormat, FourCC, MiscFlag,
    NewDxgiParams,
};
use serde::{Deserialize, Serialize};

use crate::texture::PixelFormat;

/// A kind of image file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageKind {
    Png,
    Exr,
    Dds,
    Tga,
    Bmp,
    Jpeg,
}

impl ImageKind {
    /// The kind a file's extension names.
    pub fn of_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "png" => Self::Png,
            "exr" => Self::Exr,
            "dds" => Self::Dds,
            "tga" => Self::Tga,
            "bmp" => Self::Bmp,
            "jpg" | "jpeg" => Self::Jpeg,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Exr => "exr",
            Self::Dds => "dds",
            Self::Tga => "tga",
            Self::Bmp => "bmp",
            Self::Jpeg => "jpg",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Exr => "OpenEXR",
            Self::Dds => "DDS",
            Self::Tga => "TGA",
            Self::Bmp => "BMP",
            Self::Jpeg => "JPEG",
        }
    }

    /// Whether a texture holding high dynamic range values, or 8-bit ones, goes out and comes in
    /// as this kind: a DDS holds either in the texture's own format, OpenEXR the first and the
    /// rest the second.
    pub fn holds(self, hdr: bool) -> bool {
        match self {
            Self::Dds => true,
            Self::Exr => hdr,
            _ => !hdr,
        }
    }

    /// Whether a texture goes out as this kind: a JPEG, being lossy, only comes in.
    pub fn writes(self) -> bool {
        self != Self::Jpeg
    }

    fn codec(self) -> Option<image::ImageFormat> {
        match self {
            Self::Tga => Some(image::ImageFormat::Tga),
            Self::Bmp => Some(image::ImageFormat::Bmp),
            Self::Jpeg => Some(image::ImageFormat::Jpeg),
            _ => None,
        }
    }
}

/// An image's pixels as RGBA: 8 bits, or floats from an OpenEXR file.
pub(crate) enum Pixels {
    Low(Vec<u8>),
    High(Vec<f32>),
}

/// What an image file holds.
pub(crate) enum ImageIn {
    /// Its pixels, its layers stacked top to bottom.
    Pixels {
        width: u32,
        height: u32,
        pixels: Pixels,
    },
    /// A DDS file's mips, as it stores them.
    Blocks(DdsImage),
}

/// A DDS file's pixels as it stores them, in one of the pixel formats textures use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DdsImage {
    /// The pixel format, as a texture names it.
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
    /// A cube's 6 faces, an array's slices, else 1.
    pub layers: u32,
    pub cube: bool,
    /// Each mip's layers back to back, the largest mip first.
    pub mips: Vec<Vec<u8>>,
}

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const EXR_MAGIC: &[u8] = &[0x76, 0x2f, 0x31, 0x01];

/// The image at `path`, told apart by how it starts, or for a TGA, which has no mark, by its name.
pub(crate) fn read_image(path: &Path) -> Result<ImageIn, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if bytes.starts_with(EXR_MAGIC) {
        let (width, height, rgba) = decode_exr(&bytes)?;
        return Ok(ImageIn::Pixels {
            width,
            height,
            pixels: Pixels::High(rgba),
        });
    }
    if bytes.starts_with(b"DDS ") {
        return decode_dds(&bytes).map(ImageIn::Blocks);
    }
    let (width, height, rgba) = if bytes.starts_with(PNG_MAGIC) {
        decode_png(&bytes)?
    } else if bytes.starts_with(b"BM") {
        decode_with(&bytes, ImageKind::Bmp)?
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        decode_with(&bytes, ImageKind::Jpeg)?
    } else if ImageKind::of_path(path) == Some(ImageKind::Tga) {
        decode_with(&bytes, ImageKind::Tga)?
    } else {
        return Err(format!(
            "{} is none of a PNG, OpenEXR, DDS, TGA, BMP and JPEG file",
            path.display()
        ));
    };
    Ok(ImageIn::Pixels {
        width,
        height,
        pixels: Pixels::Low(rgba),
    })
}

/// Pixels `width` by `height` as a file of `kind`, an OpenEXR file keeping them as halves where
/// `halves` says the texture holds halves.
pub(crate) fn encode_pixels(
    kind: ImageKind,
    width: u32,
    height: u32,
    pixels: &Pixels,
    halves: bool,
) -> Result<Vec<u8>, String> {
    match (kind, pixels) {
        (ImageKind::Png, Pixels::Low(rgba)) => encode_png(width, height, rgba),
        (ImageKind::Exr, Pixels::High(rgba)) => encode_exr(width, height, rgba, halves),
        (ImageKind::Tga | ImageKind::Bmp, Pixels::Low(rgba)) => {
            encode_with(width, height, rgba, kind)
        }
        _ => Err(format!("these pixels do not go in a {} file", kind.name())),
    }
}

/// A TGA, BMP or JPEG's pixels as 8-bit RGBA.
fn decode_with(bytes: &[u8], kind: ImageKind) -> Result<(u32, u32, Vec<u8>), String> {
    let codec = kind
        .codec()
        .ok_or_else(|| format!("{} is not read here", kind.name()))?;
    let rgba = image::load_from_memory_with_format(bytes, codec)
        .map_err(|e| format!("Could not read the {}: {e}", kind.name()))?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(format!("the {} has no pixels", kind.name()));
    }
    Ok((width, height, rgba.into_raw()))
}

/// 8-bit RGBA pixels as a TGA or BMP file's bytes.
fn encode_with(width: u32, height: u32, rgba: &[u8], kind: ImageKind) -> Result<Vec<u8>, String> {
    let codec = kind
        .codec()
        .ok_or_else(|| format!("{} is not written here", kind.name()))?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut out,
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
        codec,
    )
    .map_err(|e| format!("Could not write the {}: {e}", kind.name()))?;
    Ok(out.into_inner())
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

/// The DXGI format a DDS file holds a texture pixel format in.
fn dxgi_of(format: &str) -> Option<DxgiFormat> {
    Some(match format {
        "PF_DXT1" => DxgiFormat::BC1_UNorm,
        "PF_DXT3" => DxgiFormat::BC2_UNorm,
        "PF_DXT5" => DxgiFormat::BC3_UNorm,
        "PF_BC4" => DxgiFormat::BC4_UNorm,
        "PF_BC5" => DxgiFormat::BC5_UNorm,
        "PF_BC6H" => DxgiFormat::BC6H_UF16,
        "PF_BC7" => DxgiFormat::BC7_UNorm,
        "PF_R8G8B8A8" => DxgiFormat::R8G8B8A8_UNorm,
        "PF_B8G8R8A8" => DxgiFormat::B8G8R8A8_UNorm,
        "PF_G8" => DxgiFormat::R8_UNorm,
        "PF_FloatRGBA" => DxgiFormat::R16G16B16A16_Float,
        "PF_R16F" => DxgiFormat::R16_Float,
        "PF_A32B32G32R32F" => DxgiFormat::R32G32B32A32_Float,
        "PF_R32_FLOAT" => DxgiFormat::R32_Float,
        _ => return None,
    })
}

fn unread(name: &str) -> String {
    format!(
        "the DDS file holds {name}, which is none of the formats a texture takes from one: BC1 to \
         BC7 (DXT1, DXT3, DXT5), 8-bit RGBA, BGRA and grey, and half and float RGBA and red"
    )
}

/// The texture pixel format a DDS file's pixels are in. An sRGB variant stores the bytes its plain
/// one does.
fn format_of(dds: &Dds) -> Result<&'static str, String> {
    if let Some(header) = &dds.header10 {
        use DxgiFormat as D;
        return Ok(match header.dxgi_format {
            D::BC1_UNorm | D::BC1_UNorm_sRGB => "PF_DXT1",
            D::BC2_UNorm | D::BC2_UNorm_sRGB => "PF_DXT3",
            D::BC3_UNorm | D::BC3_UNorm_sRGB => "PF_DXT5",
            D::BC4_UNorm => "PF_BC4",
            D::BC5_UNorm => "PF_BC5",
            D::BC6H_UF16 => "PF_BC6H",
            D::BC7_UNorm | D::BC7_UNorm_sRGB => "PF_BC7",
            D::R8G8B8A8_UNorm | D::R8G8B8A8_UNorm_sRGB => "PF_R8G8B8A8",
            D::B8G8R8A8_UNorm | D::B8G8R8A8_UNorm_sRGB => "PF_B8G8R8A8",
            D::R8_UNorm => "PF_G8",
            D::R16G16B16A16_Float => "PF_FloatRGBA",
            D::R16_Float => "PF_R16F",
            D::R32G32B32A32_Float => "PF_A32B32G32R32F",
            D::R32_Float => "PF_R32_FLOAT",
            other => return Err(unread(&format!("{other:?}"))),
        });
    }
    const BC5U: u32 = u32::from_le_bytes(*b"BC5U");
    match dds.header.spf.fourcc.as_ref().map(|fourcc| fourcc.0) {
        Some(FourCC::DXT1) => Ok("PF_DXT1"),
        Some(FourCC::DXT3) => Ok("PF_DXT3"),
        Some(FourCC::DXT5) => Ok("PF_DXT5"),
        Some(FourCC::ATI1 | FourCC::BC4_UNORM) => Ok("PF_BC4"),
        Some(FourCC::ATI2 | BC5U) => Ok("PF_BC5"),
        Some(FourCC::A16B16G16R16F) => Ok("PF_FloatRGBA"),
        Some(FourCC::R16F) => Ok("PF_R16F"),
        Some(FourCC::A32B32G32R32F) => Ok("PF_A32B32G32R32F"),
        Some(FourCC::R32F) => Ok("PF_R32_FLOAT"),
        Some(code) => {
            let text = code.to_le_bytes();
            Err(unread(&if text.iter().all(u8::is_ascii_alphanumeric) {
                String::from_utf8_lossy(&text).into_owned()
            } else {
                format!("format {code}")
            }))
        }
        // By its masks alone: writers disagree on the flags an 8-bit grey file carries.
        None => {
            let spf = &dds.header.spf;
            let none = |mask: Option<u32>| mask.is_none_or(|mask| mask == 0);
            match (
                spf.rgb_bit_count,
                spf.r_bit_mask,
                spf.g_bit_mask,
                spf.b_bit_mask,
            ) {
                (Some(32), Some(0xff), Some(0xff00), Some(0xff_0000))
                    if spf.a_bit_mask == Some(0xff00_0000) =>
                {
                    Ok("PF_R8G8B8A8")
                }
                (Some(32), Some(0xff_0000), Some(0xff00), Some(0xff))
                    if spf.a_bit_mask == Some(0xff00_0000) =>
                {
                    Ok("PF_B8G8R8A8")
                }
                (Some(8), Some(0xff), g, b) if none(g) && none(b) && none(spf.a_bit_mask) => {
                    Ok("PF_G8")
                }
                _ => Err(unread(&match D3DFormat::try_from_pixel_format(spf) {
                    Some(format) => format!("{format:?}"),
                    None => "a pixel format it gives by masks".into(),
                })),
            }
        }
    }
}

/// The bytes one layer of each of `count` mips takes, from one `width` by `height` down.
fn chain_sizes(format: PixelFormat, width: u32, height: u32, count: usize) -> Vec<usize> {
    (0..count as u32)
        .map(|mip| {
            let shrink = |side: u32| side.checked_shr(mip).unwrap_or(0).max(1);
            format.mip_size(shrink(width), shrink(height))
        })
        .collect()
}

/// A DDS file's mips, regrouped from the file's order, each layer's whole chain in turn, to a
/// texture's, each mip's layers in turn.
pub(crate) fn decode_dds(bytes: &[u8]) -> Result<DdsImage, String> {
    let dds = Dds::read(bytes).map_err(|e| format!("Could not read the DDS file: {e}"))?;
    let volume = dds.header.caps2.contains(Caps2::VOLUME)
        || dds.header.depth.is_some_and(|depth| depth > 1)
        || dds.header10.as_ref().is_some_and(|header| {
            matches!(header.resource_dimension, D3D10ResourceDimension::Texture3D)
        });
    if volume {
        return Err(
            "the DDS file is a volume: give a volume texture its depth slices stacked top to \
             bottom in a PNG or OpenEXR file"
                .into(),
        );
    }
    let format = format_of(&dds)?;
    let (width, height) = (dds.get_width(), dds.get_height());
    if width == 0 || height == 0 {
        return Err("the DDS file has no pixels".into());
    }
    let cube = dds.header.caps2.contains(Caps2::CUBEMAP)
        || dds
            .header10
            .as_ref()
            .is_some_and(|header| header.misc_flag.contains(MiscFlag::TEXTURECUBE));
    let layers = dds.get_num_array_layers().max(1);
    let full = (32 - width.max(height).leading_zeros()) as usize;
    let count = (dds.get_num_mipmap_levels().max(1) as usize).min(full);
    let sizes = chain_sizes(PixelFormat::of(format)?, width, height, count);
    let chain: usize = sizes.iter().sum();
    let wanted = chain * layers as usize;
    if dds.data.len() != wanted {
        return Err(format!(
            "the DDS file holds {} bytes of pixels, not the {wanted} of its {layers} {width}x{height} \
             layer(s) of {count} mip(s)",
            dds.data.len()
        ));
    }
    let mut mips: Vec<Vec<u8>> = sizes
        .iter()
        .map(|size| Vec::with_capacity(size * layers as usize))
        .collect();
    for layer in dds.data.chunks_exact(chain) {
        let mut at = 0;
        for (mip, size) in mips.iter_mut().zip(&sizes) {
            mip.extend_from_slice(&layer[at..at + size]);
            at += size;
        }
    }
    Ok(DdsImage {
        format,
        width,
        height,
        layers,
        cube,
        mips,
    })
}

/// A DDS file holding a texture's mips as they are, in its pixel format: `mips` largest first,
/// from one `width` by `height` down, each holding its `layers` back to back.
pub(crate) fn encode_dds(
    format: &str,
    width: u32,
    height: u32,
    layers: u32,
    cube: bool,
    mips: &[&[u8]],
) -> Result<Vec<u8>, String> {
    let dxgi = dxgi_of(format).ok_or_else(|| format!("{format} has no DDS format"))?;
    let sizes = chain_sizes(PixelFormat::of(format)?, width, height, mips.len());
    for (index, (mip, size)) in mips.iter().zip(&sizes).enumerate() {
        if mip.len() != size * layers as usize {
            return Err(format!(
                "mip {index} holds {} bytes, not the {} of {layers} layer(s)",
                mip.len(),
                size * layers as usize
            ));
        }
    }
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height,
        width,
        depth: None,
        format: dxgi,
        mipmap_levels: Some(mips.len() as u32),
        array_layers: Some(layers),
        caps2: None,
        is_cubemap: cube,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Unknown,
    })
    .map_err(|e| format!("Could not write the DDS file: {e}"))?;
    let mut data = Vec::with_capacity(dds.data.len());
    for layer in 0..layers as usize {
        for (mip, size) in mips.iter().zip(&sizes) {
            data.extend_from_slice(&mip[layer * size..(layer + 1) * size]);
        }
    }
    if data.len() != dds.data.len() {
        return Err(format!(
            "the DDS file's header gives {} bytes of pixels, not the {} written",
            dds.data.len(),
            data.len()
        ));
    }
    dds.data = data;
    let mut out = Vec::with_capacity(dds.data.len() + 148);
    dds.write(&mut out)
        .map_err(|e| format!("Could not write the DDS file: {e}"))?;
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const FORMATS: [&str; 14] = [
        "PF_DXT1",
        "PF_DXT3",
        "PF_DXT5",
        "PF_BC4",
        "PF_BC5",
        "PF_BC6H",
        "PF_BC7",
        "PF_R8G8B8A8",
        "PF_B8G8R8A8",
        "PF_G8",
        "PF_FloatRGBA",
        "PF_R16F",
        "PF_A32B32G32R32F",
        "PF_R32_FLOAT",
    ];

    /// Mips of distinct bytes, each holding `layers` layers.
    fn mips(format: &str, width: u32, height: u32, layers: u32, count: usize) -> Vec<Vec<u8>> {
        let sizes = chain_sizes(PixelFormat::of(format).unwrap(), width, height, count);
        sizes
            .iter()
            .enumerate()
            .map(|(mip, size)| {
                (0..size * layers as usize)
                    .map(|i| (i * 7 + mip * 13) as u8)
                    .collect()
            })
            .collect()
    }

    fn round_trip(
        format: &'static str,
        width: u32,
        height: u32,
        layers: u32,
        cube: bool,
        count: usize,
    ) {
        let mips = mips(format, width, height, layers, count);
        let slices: Vec<&[u8]> = mips.iter().map(Vec::as_slice).collect();
        let file = encode_dds(format, width, height, layers, cube, &slices).unwrap();
        let read = decode_dds(&file).unwrap();
        assert_eq!(
            read,
            DdsImage {
                format,
                width,
                height,
                layers,
                cube,
                mips,
            },
            "{format}"
        );
    }

    #[test]
    fn every_format_goes_to_a_dds_file_and_back_as_it_was() {
        for format in FORMATS {
            round_trip(format, 64, 32, 1, false, 7);
            round_trip(format, 6, 10, 1, false, 4);
            round_trip(format, 16, 16, 6, true, 5);
            round_trip(format, 8, 8, 3, false, 1);
        }
    }

    fn legacy(format: D3DFormat, width: u32, height: u32, depth: Option<u32>) -> Vec<u8> {
        let mut dds = Dds::new_d3d(ddsfile::NewD3dParams {
            height,
            width,
            depth,
            format,
            mipmap_levels: Some(2),
            caps2: depth.map(|_| Caps2::VOLUME),
        })
        .unwrap();
        // The library writes a float format by masks, where the tools that make DDS files give
        // its code.
        if format == D3DFormat::A16B16G16R16F {
            dds.header.spf.fourcc = Some(FourCC(FourCC::A16B16G16R16F));
            dds.header.spf.flags = ddsfile::PixelFormatFlags::FOURCC;
        }
        for (i, byte) in dds.data.iter_mut().enumerate() {
            *byte = i as u8;
        }
        let mut out = Vec::new();
        dds.write(&mut out).unwrap();
        out
    }

    #[test]
    fn a_legacy_dds_file_reads_in_the_format_its_code_or_masks_name() {
        for (format, named) in [
            (D3DFormat::DXT1, "PF_DXT1"),
            (D3DFormat::DXT5, "PF_DXT5"),
            (D3DFormat::A8R8G8B8, "PF_B8G8R8A8"),
            (D3DFormat::A8B8G8R8, "PF_R8G8B8A8"),
            (D3DFormat::L8, "PF_G8"),
            (D3DFormat::A16B16G16R16F, "PF_FloatRGBA"),
        ] {
            let read = decode_dds(&legacy(format, 8, 8, None))
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!((read.format, read.mips.len(), read.layers), (named, 2, 1));
            let first = PixelFormat::of(named).unwrap().mip_size(8, 8);
            assert_eq!(
                read.mips[0],
                (0..first).map(|i| i as u8).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_dds_file_that_no_texture_takes_says_why() {
        let volume = decode_dds(&legacy(D3DFormat::A8R8G8B8, 4, 4, Some(4))).unwrap_err();
        assert!(volume.contains("volume"), "{volume}");
        let unread = decode_dds(&legacy(D3DFormat::R5G6B5, 4, 4, None)).unwrap_err();
        assert!(unread.contains("R5G6B5"), "{unread}");
        let mips = mips("PF_DXT1", 8, 8, 1, 2);
        let slices: Vec<&[u8]> = mips.iter().map(Vec::as_slice).collect();
        let mut file = encode_dds("PF_DXT1", 8, 8, 1, false, &slices).unwrap();
        file.truncate(file.len() - 1);
        let short = decode_dds(&file).unwrap_err();
        assert!(short.contains("bytes of pixels"), "{short}");
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rivals-image-file-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tga_and_bmp_files_read_back_the_pixels_written() {
        let dir = scratch("tga-bmp");
        let rgba: Vec<u8> = (0..5 * 3 * 4).map(|i| (i * 11) as u8).collect();
        for kind in [ImageKind::Tga, ImageKind::Bmp, ImageKind::Png] {
            let path = dir.join(format!("pixels.{}", kind.extension()));
            let file = encode_pixels(kind, 5, 3, &Pixels::Low(rgba.clone()), false).unwrap();
            std::fs::write(&path, file).unwrap();
            match read_image(&path).unwrap() {
                ImageIn::Pixels {
                    width: 5,
                    height: 3,
                    pixels: Pixels::Low(back),
                } => assert_eq!(back, rgba, "{kind:?}"),
                _ => panic!("{kind:?} read back as something else"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_jpeg_reads_as_its_pixels_near_enough() {
        let dir = scratch("jpeg");
        let rgb: Vec<u8> = (0..16 * 16).flat_map(|_| [200u8, 40, 90]).collect();
        let mut file = std::io::Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut file,
            &rgb,
            16,
            16,
            image::ExtendedColorType::Rgb8,
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        let path = dir.join("flat.jpg");
        std::fs::write(&path, file.into_inner()).unwrap();
        let ImageIn::Pixels {
            width: 16,
            height: 16,
            pixels: Pixels::Low(back),
        } = read_image(&path).unwrap()
        else {
            panic!("the JPEG read back as something else");
        };
        for pixel in back.as_chunks::<4>().0 {
            for (got, wanted) in pixel.iter().zip([200u8, 40, 90, 255]) {
                assert!(got.abs_diff(wanted) <= 4, "{pixel:?}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_of_no_kind_read_here_is_refused_by_name() {
        let dir = scratch("unknown");
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"not an image").unwrap();
        let refused = read_image(&path).map(|_| ()).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(refused.contains("none of a PNG"), "{refused}");
        assert!(encode_pixels(ImageKind::Tga, 1, 1, &Pixels::High(vec![0.0; 4]), true).is_err());
    }
}
