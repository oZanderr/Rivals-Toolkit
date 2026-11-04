//! Where a package cooked by a stock UE 5.3 editor lacks bytes this game's engine reads, and what
//! a conversion writes there.

use serde::Serialize;

/// One place a stock cook stops short of this game's layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StockGap {
    pub export: u32,
    /// The file offset the missing bytes go in at.
    pub at: u64,
    #[serde(flatten)]
    pub kind: StockGapKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "gap", rename_all = "snake_case")]
pub enum StockGapKind {
    /// A skeletal mesh material slot, which this game follows with its gameplay tags.
    MaterialTags { slot: String },
    /// A static mesh LOD screen size, which this game follows with the distance it switches to
    /// that LOD at. `lods` is how many LODs the mesh has.
    ScreenSize { lod: u32, lods: u32 },
    /// A string table entry, which this game follows with a marker string.
    StringMarker { key: String },
    /// Static mesh render data a conversion cannot take to this game's, or could not read through.
    RenderData { found: StockRender },
}

/// What a stock static mesh's render data holds that this game's engine cannot take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "render", rename_all = "snake_case")]
pub enum StockRender {
    Nanite,
    /// The editor's distance field, where this game reads one in its own format.
    DistanceField {
        lod: u32,
    },
    /// Render data laid out in a way the read does not follow, so neither was looked for.
    Unread {
        why: String,
    },
}

impl StockRender {
    /// The editor setting a recook turns off, for what no conversion can fix.
    pub fn fix(&self) -> Option<&'static str> {
        match self {
            Self::Nanite => Some("Nanite off on the mesh"),
            Self::DistanceField { .. } => Some(
                "Generate Mesh Distance Fields off in the project (`r.GenerateMeshDistanceFields=False`)",
            ),
            Self::Unread { .. } => None,
        }
    }
}

/// The distance in centimetres this game's own static meshes switch to LODs 1, 2 and 3 at, whatever
/// their size: 15, 45 and 130 m.
const LOD_DISTANCES: [f32; 3] = [1500.0, 4500.0, 13000.0];

/// The distance this game holds for a LOD it never switches to by distance, 2^43.
const NEVER: f32 = 8_796_093_022_208.0;

/// How far from the camera this game switches a static mesh with `lods` LODs to LOD `lod`. It
/// picks a static mesh's LOD by distance (`lod.EnableDistance`, set at every View Distance quality
/// but Cine) and not by screen size, so a LOD given its own meshes' 2^43 is never drawn but at a
/// low View Distance, whose LOD bias skips LOD 0. LOD 0 gets the game's 0, LODs 1 to 3 its 15, 45
/// and 130 m, each further LOD three times the one before, and the slots past the last LOD 2^43.
pub fn lod_distance(lod: u32, lods: u32) -> f32 {
    match lod {
        0 => 0.0,
        _ if lod >= lods => NEVER,
        _ => match LOD_DISTANCES.get(lod as usize - 1) {
            Some(distance) => *distance,
            None => LOD_DISTANCES[2] * 3f32.powi(lod as i32 - 3),
        },
    }
}

impl StockGapKind {
    /// The bytes that fill the gap: an empty tag container, an empty string, or the cooked
    /// per-platform distance the game switches to a LOD at, which a stock cook does not carry.
    pub fn filler(&self) -> Vec<u8> {
        match self {
            Self::MaterialTags { .. } | Self::StringMarker { .. } => vec![0; 4],
            Self::ScreenSize { lod, lods } => {
                let mut bytes = 1u32.to_le_bytes().to_vec();
                bytes.extend(lod_distance(*lod, *lods).to_le_bytes());
                bytes
            }
            Self::RenderData { .. } => Vec::new(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::MaterialTags { slot } => format!("material slot {slot} has no gameplay tags"),
            Self::ScreenSize { lod, .. } => {
                format!("LOD {lod}'s screen size is eight bytes short")
            }
            Self::StringMarker { key } => format!("string table entry {key} has no marker"),
            Self::RenderData {
                found: StockRender::Nanite,
            } => "its static mesh holds Nanite data, which the game does not draw".into(),
            Self::RenderData {
                found: StockRender::DistanceField { lod },
            } => format!(
                "its static mesh's LOD {lod} holds the editor's distance field, which this game \
                 reads in its own format"
            ),
            Self::RenderData {
                found: StockRender::Unread { why },
            } => format!(
                "its static mesh's render data could not be read through ({why}), so it was not \
                 checked for distance fields or Nanite"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the game's own meshes hold them: none for LOD 0, 15, 45 and 130 m after, and the
    /// never-switch value only past the mesh's last LOD.
    #[test]
    fn each_real_lod_gets_the_distance_the_game_switches_at() {
        let distances: Vec<f32> = (0..8).map(|lod| lod_distance(lod, 5)).collect();
        assert_eq!(
            distances,
            [0.0, 1500.0, 4500.0, 13000.0, 39000.0, NEVER, NEVER, NEVER]
        );
        let filler = StockGapKind::ScreenSize { lod: 2, lods: 3 }.filler();
        assert_eq!(filler[..4], 1u32.to_le_bytes());
        assert_eq!(filler[4..], 4500f32.to_le_bytes());
        let padding = StockGapKind::ScreenSize { lod: 3, lods: 3 }.filler();
        assert_eq!(padding, [1, 0, 0, 0, 0, 0, 0, 0x55]);
    }
}
