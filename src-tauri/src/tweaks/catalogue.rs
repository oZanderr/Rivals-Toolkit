//! Tweak catalogue types and the full list of supported scalability tweaks.

use serde::{Deserialize, Serialize};

/// One line pattern used by a `RemoveLines` tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakLine {
    pub pattern: String,
    /// Target scalability section for re-adding (e.g. `PostProcessQuality@0`).
    /// When `None`, this line is never re-added to the scalability file on disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalability_section: Option<String>,
    /// Target Engine.ini section for pak edits (defaults to `[ConsoleVariables]` when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
    /// When set, replace the matched line with this string instead of removing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_with: Option<String>,
}

/// One key/value assignment used by a `BatchToggle` tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BatchToggleEntry {
    pub key: String,
    pub on_value: String,
    /// Value to write when disabled. If absent, the key is removed from the file instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_value: Option<String>,
    /// Target scalability section (e.g. `PostProcessQuality@0`).
    /// `None` for pak-only tweaks that don't touch scalability files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalability_section: Option<String>,
    /// Optional Engine.ini section override for pak edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
}

/// Tweak behavior definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum TweakKind {
    RemoveLines {
        lines: Vec<TweakLine>,
        /// When true, lines are only removed and can never be restored.
        #[serde(default)]
        remove_only: bool,
    },
    Toggle {
        key: String,
        on_value: String,
        /// Value to write when disabled. If absent, the key is removed from the file instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        off_value: Option<String>,
        /// Active-state fallback when the key is absent.
        #[serde(default)]
        default_enabled: bool,
        /// Target scalability section (e.g. `PostProcessQuality@0`).
        /// `None` for pak-only tweaks that don't touch scalability files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scalability_section: Option<String>,
        /// Target Engine.ini section for pak edits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine_section: Option<String>,
    },
    BatchToggle {
        entries: Vec<BatchToggleEntry>,
        /// Active-state fallback when keys are absent.
        #[serde(default)]
        default_enabled: bool,
    },
    Slider {
        key: String,
        min: f64,
        max: f64,
        step: f64,
        default_value: f64,
        /// When true, disabling the slider writes `default_value` back instead of removing the key.
        #[serde(default)]
        write_default_on_disable: bool,
        /// Target scalability section (e.g. `PostProcessQuality@0`).
        /// `None` for pak-only tweaks that don't touch scalability files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scalability_section: Option<String>,
        /// Target Engine.ini section for pak edits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine_section: Option<String>,
    },
}

/// User-facing tweak definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakDefinition {
    pub id: String,
    pub label: String,
    pub category: String,
    pub description: String,
    #[serde(default)]
    pub pak_only: bool,
    #[serde(flatten)]
    pub kind: TweakKind,
}

/// Detected state of a tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakState {
    pub id: String,
    pub active: bool,
    pub current_value: Option<String>,
}

/// Requested setting for a tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakSetting {
    pub id: String,
    pub enabled: bool,
    pub value: Option<String>,
}

/// Build the tweak catalogue.
pub(crate) fn tweak_catalogue() -> Vec<TweakDefinition> {
    vec![
        // Gameplay Fixes
        TweakDefinition {
            id: "fix_abilities".into(),
            label: "Fix Chronovision / Punisher / Hela Walls".into(),
            category: "Gameplay Fixes".into(),
            description: "Restores Punisher passive wallhack, Hela wall-vision in ult, and \
                           Chronovision by removing post-process material overrides. \
                           r.CustomDepth and r.LightTile.Enable only take effect in pak mods."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.PostProcessing.DisableMaterials=1".into(), scalability_section: Some("PostProcessQuality@0".into()), engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.CustomDepth=0".into(),                     scalability_section: None, engine_section: None, replace_with: Some("r.CustomDepth=3".into()) },
                    TweakLine { pattern: "r.LightTile.Enable=0".into(),                scalability_section: None, engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_low_res_doom".into(),
            label: "Fix Low-Res Doom Match".into(),
            category: "Gameplay Fixes".into(),
            description: "Removes the screen percentage lower limit that causes very low \
                           resolution in Doom Match."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "m.Portal.ScreenPercentageLowerLimit=1".into(), scalability_section: Some("EffectsQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_dead_bodies".into(),
            label: "Fix Dead Bodies Falling Through Floor".into(),
            category: "Gameplay Fixes".into(),
            description: "Removes the line that disables simulation collision, fixing dead \
                           bodies clipping through the floor."
                .into(),
            pak_only: true,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "p.SimCollisionEnabled=0".into(), scalability_section: None, engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_smoke_outlines".into(),
            label: "Fix Missing Smoke & Outlines".into(),
            category: "Gameplay Fixes".into(),
            description: "Restores Punisher smoke, Bruce Banner smoke, and Bucky ult outlines \
                           by re-enabling Niagara sprite rendering."
                .into(),
            pak_only: true,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "fx.EnableNiagaraSpriteRendering=0".into(), scalability_section: None, engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_kun_lun_smoke".into(),
            label: "Fix Kun-Lun Smoke".into(),
            category: "Gameplay Fixes".into(),
            description: "Removes distance field CVars that disable smoke effects on Kun-Lun."
                .into(),
            pak_only: true,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.StaticMesh.StripDistanceFieldDataDuringLoad=1".into(),      scalability_section: None, engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.StaticMesh.StripDistanceFieldDataOnLoad=1".into(),          scalability_section: None, engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.DistanceFields=0".into(),                                   scalability_section: None, engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.DistanceFields.MaxObjectBoundingRadius=0.0000001".into(),   scalability_section: None, engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.AlwaysPrepareGlobalDistaneField=0".into(),                  scalability_section: None, engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.AlwaysPrepareGlobalDistanceField=0".into(),                 scalability_section: None, engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.GenerateMeshDistanceFields=0".into(),                       scalability_section: None, engine_section: None, replace_with: Some("r.GenerateMeshDistanceFields=True".into()) },
                ],
                remove_only: true,
            },
        },
        // Lighting & Color
        TweakDefinition {
            id: "fix_dark_maps".into(),
            label: "Fix Dark Maps".into(),
            category: "Lighting & Color".into(),
            description: "Removes aggressive light distance culling that causes maps to appear \
                           too dark."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.LightMaxDrawDistanceScale=0.00000001".into(), scalability_section: Some("ShadowQuality@0".into()), engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.LightFadeDistance=1".into(),                  scalability_section: Some("GlobalIlluminationQuality@0".into()), engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.LightCullingDistance=1".into(),               scalability_section: Some("GlobalIlluminationQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "brightness".into(),
            label: "Brightness (Tonemapper Gamma)".into(),
            category: "Lighting & Color".into(),
            description: "Adjusts overall scene brightness via tonemapper gamma. \
                           Higher values = brighter."
                .into(),
            pak_only: false,
            kind: TweakKind::Slider {
                key: "r.TonemapperGamma".into(),
                min: 0.0,
                max: 5.0,
                step: 0.1,
                default_value: 2.2,
                write_default_on_disable: false,
                scalability_section: Some("PostProcessQuality@0".into()),
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "fix_color_banding".into(),
            label: "Fix Color Banding".into(),
            category: "Lighting & Color".into(),
            description: "Removes the scene color format override that causes visible color \
                           banding. Especially recommended when using the Quake Environments mod."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.SceneColorFormat=0".into(), scalability_section: Some("EffectsQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_dark_characters".into(),
            label: "Fix Incorrect Dark Characters".into(),
            category: "Lighting & Color".into(),
            description: "Removes the eye adaptation disable that causes characters to render \
                           incorrectly dark."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.PostProcessing.EnableEyeAdaptation=0".into(), scalability_section: Some("PostProcessQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_unsaturated_hp".into(),
            label: "Fix Unsaturated HP Bars".into(),
            category: "Lighting & Color".into(),
            description: "Removes the separate translucency disable that causes HP bars to \
                           appear unsaturated."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.SeparateTranslucency=0".into(), scalability_section: Some("PostProcessQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        // Sharpness & Textures
        TweakDefinition {
            id: "cas_sharpening".into(),
            label: "CAS Sharpening".into(),
            category: "Sharpness & Textures".into(),
            description: "Contrast Adaptive Sharpening. Disable for a softer/blurrier look, \
                           enable for a sharper image."
                .into(),
            pak_only: false,
            kind: TweakKind::Toggle {
                key: "r.PostProcessing.EnableCAS".into(),
                on_value: "1".into(),
                off_value: Some("0".into()),
                default_enabled: true,
                scalability_section: Some("PostProcessQuality@0".into()),
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "fix_black_hair".into(),
            label: "Fix Black Hair".into(),
            category: "Sharpness & Textures".into(),
            description: "Removes anisotropic material overrides that cause characters to \
                           have black hair. For full pitch-black hair on all characters, \
                           keep DLSS off and enable Fix Color Banding."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.AnisotropicMaterials=0".into(),    scalability_section: Some("ShadingQuality@0".into()), engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.VT.AnisotropicMaterials=0".into(), scalability_section: Some("TextureQuality@0".into()), engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.VT.MaxAnisotropy=0".into(),        scalability_section: Some("TextureQuality@0".into()), engine_section: None, replace_with: None },
                    TweakLine { pattern: "r.MaxAnisotropy=0".into(),           scalability_section: Some("TextureQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_mipmap_bias".into(),
            label: "Fix MipMap LOD Bias".into(),
            category: "Sharpness & Textures".into(),
            description: "Removes high MipMap LOD bias that degrades texture quality.".into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.MipMapLODBias=15".into(), scalability_section: Some("TextureQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_streaming_bias".into(),
            label: "Fix Streaming Mip Bias".into(),
            category: "Sharpness & Textures".into(),
            description: "Removes streaming mip bias that can cause blurry textures.".into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.Streaming.MipBias=2".into(), scalability_section: Some("TextureQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        TweakDefinition {
            id: "fix_streaming_pool".into(),
            label: "Fix Streaming Pool Size".into(),
            category: "Sharpness & Textures".into(),
            description: "Removes restricted streaming pool size. \
                           Disable this fix to keep the line for Quake Environments mod compatibility."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    TweakLine { pattern: "r.Streaming.PoolSize=1".into(), scalability_section: Some("TextureQuality@0".into()), engine_section: None, replace_with: None },
                ],
                remove_only: false,
            },
        },
        // Display
        TweakDefinition {
            id: "application_scale".into(),
            label: "Application Scale".into(),
            category: "Display".into(),
            description: "Controls UI / viewport scaling. Default is 1.0. Values above 1.0 \
                           zoom in (useful for 4:3 without black bars). \
                           Only effective as an engine pak mod."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "ApplicationScale".into(),
                min: 0.5,
                max: 2.0,
                step: 0.05,
                default_value: 1.0,
                write_default_on_disable: false,
                scalability_section: None,
                engine_section: Some("/Script/Engine.UserInterfaceSettings".into()),
            },
        },
        TweakDefinition {
            id: "font_aa".into(),
            label: "Font Anti-Aliasing".into(),
            category: "Display".into(),
            description: "Toggles anti-aliasing on UI text / fonts. Not much performance change, mostly stylistic preference.".into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "Slate.EnableFontAntiAliasing".into(),
                on_value: "1".into(),
                off_value: Some("0".into()),
                default_enabled: true,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "character_outlines".into(),
            label: "Character Outlines".into(),
            category: "Display".into(),
            description: "Controls stencil-based outlines on characters, including \
                           team and enemy highlights. Disable to remove all outline/highlighting effects."
                .into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "StencilComponent.EnableOutline".into(),
                on_value: "1".into(),
                off_value: Some("0".into()),
                default_enabled: true,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "team_outline_line_mode".into(),
            label: "Team Outline Line Mode".into(),
            category: "Display".into(),
            description: "Controls the rendering mode used for team/enemy outlines. \
                           Different modes can noticeably change edge behavior and overall style; visual impact is greater at lower resolutions."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "r.TeamOutline.LineMode".into(),
                min: 0.0,
                max: 2.0,
                step: 1.0,
                default_value: 1.0,
                write_default_on_disable: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "team_outline_line_width".into(),
            label: "Team Outline Line Width".into(),
            category: "Display".into(),
            description: "Controls team/enemy outline thickness. \
                           Higher values draw thicker lines; \
                           visual impact is greater at lower resolutions."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "r.TeamOutline.LineWidth".into(),
                min: 0.0,
                max: 2.0,
                step: 1.0,
                default_value: 1.0,
                write_default_on_disable: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "hide_marvel_widget_ui".into(),
            label: "Hide Overhead HP/Name UI".into(),
            category: "Display".into(),
            description: "Hides the UI for overhead health bars and player names. Potentially helpful for those on low-end hardware."
                .into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "UI.HideMarvelWidgetUI".into(),
                on_value: "1".into(),
                off_value: Some("0".into()),
                default_enabled: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        // Latency
        TweakDefinition {
            id: "latency_reflex_mode".into(),
            label: "NVIDIA Reflex Mode".into(),
            category: "Latency".into(),
            description: "Controls NVIDIA Reflex mode via Streamline. \
                           0 = Off, 1 = On, 2 = On + Boost. \
                           Mode 2 can have weird issues like massive \
                           performance loss in certain areas of the firing range."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "t.Streamline.Reflex.Mode".into(),
                min: 0.0,
                max: 2.0,
                step: 1.0,
                default_value: 0.0,
                write_default_on_disable: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_gt_sync_type".into(),
            label: "Game Thread Sync Type".into(),
            category: "Latency".into(),
            description: "Controls how the game thread synchronises with the GPU pipeline, \
                           affecting input latency. \
                           0 = sync to render thread (highest latency); \
                           1 = sync to RHI thread (game default, balanced); \
                           2 = sync to GPU swap-chain flip (lowest latency, if compatible). \
                           Toggle is active only when set away from the default."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "r.GTSyncType".into(),
                min: 0.0,
                max: 2.0,
                step: 1.0,
                default_value: 1.0,
                write_default_on_disable: true,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_finish_current_frame".into(),
            label: "Finish Current Frame".into(),
            category: "Latency".into(),
            description: "Forces the current frame to finish/present instead of buffering. \
                           Can improve input latency, but usually reduces \
                           overall performance."
                .into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "r.FinishCurrentFrame".into(),
                on_value: "1".into(),
                off_value: Some("0".into()),
                default_enabled: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_one_frame_thread_lag".into(),
            label: "One-Frame Thread Lag".into(),
            category: "Latency".into(),
            description: "Controls whether the render thread lags one frame behind the game \
                           thread. Disable this to reduce latency at a \
                           cost of performance."
                .into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "r.OneFrameThreadLag".into(),
                on_value: "1".into(),
                off_value: Some("0".into()),
                default_enabled: true,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_sync_interval".into(),
            label: "VSync Sync Interval".into(),
            category: "Latency".into(),
            description: "Controls present interval for VSync-capable RHIs: 0 = present \
                           immediately, 1 = every vblank, 2 = every 2 vblanks, \
                           etc. Higher values generally increase latency and lower frame rate. Only effective with VSync on."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "rhi.SyncInterval".into(),
                min: 0.0,
                max: 4.0,
                step: 1.0,
                default_value: 0.0,
                write_default_on_disable: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_sync_allow_early_kick".into(),
            label: "Sync Allow Early Kick".into(),
            category: "Latency".into(),
            description: "With VSync on, lets the game start the next frame a bit earlier so \
                           input feels closer to VSync off. Only matters when VSync is enabled. \
                           May cause minor stutter if your frame rate is unstable."
                .into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "rhi.SyncAllowEarlyKick".into(),
                on_value: "1".into(),
                off_value: None,
                default_enabled: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_sync_slack_ms".into(),
            label: "Sync Slack (ms)".into(),
            category: "Latency".into(),
            description: "Extra wait time (in milliseconds) before a frame is considered late. \
                           Lower values feel more responsive but can cause more stutter if your \
                           frame rate is unstable. Only matters when VSync is on or you're using \
                           G-Sync / FreeSync."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "rhi.SyncSlackMS".into(),
                min: 0.0,
                max: 30.0,
                step: 1.0,
                default_value: 0.0,
                write_default_on_disable: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        // Experimental
        TweakDefinition {
            id: "black_skyboxes".into(),
            label: "Black Skyboxes".into(),
            category: "Experimental".into(),
            description: "Sets r.ViewDistanceScale to an extremely low value, culling distant \
                           world geometry and backgrounds. Can improve performance on low-end \
                           hardware at the cost of environmental depth."
                .into(),
            pak_only: false,
            kind: TweakKind::Toggle {
                key: "r.ViewDistanceScale".into(),
                on_value: "0.0000000000000000000000000000000001".into(),
                off_value: None,
                default_enabled: false,
                scalability_section: Some("ViewDistanceQuality@0".into()),
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "force_default_material".into(),
            label: "Force Default Material".into(),
            category: "Experimental".into(),
            description: "Replaces all character materials with the engine default, \
                           stripping textures and skins. Can improve performance on very low-end \
                           hardware at the cost of visual clarity."
                .into(),
            pak_only: true,
            kind: TweakKind::Toggle {
                key: "r.debug.ForceDefaultMtl".into(),
                on_value: "1".into(),
                off_value: None,
                default_enabled: false,
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "network_revert_update_65".into(),
            label: "Revert Network Rates (Update 6.5+)".into(),
            category: "Experimental".into(),
            description: "Reverts update 6.5+ network rate changes that can cause \
                           teleporting/desync on some setups. Applies all four values in Engine.ini.".into(),
            pak_only: true,
            kind: TweakKind::BatchToggle {
                entries: vec![
                    BatchToggleEntry {
                        key: "MaxClientRate".into(),
                        on_value: "300000".into(),
                        off_value: Some("8000000".into()),
                        scalability_section: None,
                        engine_section: Some("/Script/OnlineSubsystemUtils.IpNetDriver".into()),
                    },
                    BatchToggleEntry {
                        key: "MaxInternetClientRate".into(),
                        on_value: "300000".into(),
                        off_value: Some("8000000".into()),
                        scalability_section: None,
                        engine_section: Some("/Script/OnlineSubsystemUtils.IpNetDriver".into()),
                    },
                    BatchToggleEntry {
                        key: "ConfiguredInternetSpeed".into(),
                        on_value: "300000".into(),
                        off_value: Some("10000000".into()),
                        scalability_section: None,
                        engine_section: Some("/Script/Engine.Player".into()),
                    },
                    BatchToggleEntry {
                        key: "ConfiguredLanSpeed".into(),
                        on_value: "300000".into(),
                        off_value: Some("15000000".into()),
                        scalability_section: None,
                        engine_section: Some("/Script/Engine.Player".into()),
                    },
                ],
                default_enabled: false,
            },
        },
    ]
}

const GUS_MARVEL: &str = "/Script/Marvel.MarvelGameUserSettings";
const GUS_SCALABILITY: &str = "ScalabilityGroups";

/// Curated tweaks for the user's saved GameUserSettings.ini. Sections differ from
/// the system Scalability.ini, so this is a separate catalogue.
pub(crate) fn game_user_settings_catalogue() -> Vec<TweakDefinition> {
    let toggle = |id: &str,
                  label: &str,
                  category: &str,
                  description: &str,
                  key: &str,
                  default_enabled: bool|
     -> TweakDefinition {
        TweakDefinition {
            id: id.into(),
            label: label.into(),
            category: category.into(),
            description: description.into(),
            pak_only: false,
            kind: TweakKind::Toggle {
                key: key.into(),
                on_value: "True".into(),
                off_value: Some("False".into()),
                default_enabled,
                scalability_section: Some(GUS_MARVEL.into()),
                engine_section: None,
            },
        }
    };
    let slider = |id: &str,
                  label: &str,
                  category: &str,
                  description: &str,
                  key: &str,
                  section: &str,
                  min: f64,
                  max: f64,
                  step: f64,
                  default_value: f64|
     -> TweakDefinition {
        TweakDefinition {
            id: id.into(),
            label: label.into(),
            category: category.into(),
            description: description.into(),
            pak_only: false,
            kind: TweakKind::Slider {
                key: key.into(),
                min,
                max,
                step,
                default_value,
                write_default_on_disable: true,
                scalability_section: Some(section.into()),
                engine_section: None,
            },
        }
    };
    vec![
        // ── Latency ──
        toggle(
            "gus_vsync",
            "VSync",
            "Latency",
            "Vertical sync. Eliminates tearing at the cost of input latency.",
            "bUseVSync",
            false,
        ),
        toggle(
            "gus_dynamic_res",
            "Dynamic Resolution",
            "Latency",
            "Dynamically lowers internal resolution to maintain frame rate.",
            "bUseDynamicResolution",
            false,
        ),
        toggle(
            "gus_nvidia_reflex",
            "Nvidia Reflex",
            "Latency",
            "Low-latency mode for Nvidia GPUs.",
            "bNvidiaReflex",
            false,
        ),
        toggle(
            "gus_amd_anti_lag",
            "AMD Anti-Lag 2",
            "Latency",
            "Low-latency mode for AMD GPUs.",
            "bAMDAntiLag2",
            false,
        ),
        toggle(
            "gus_xe_low_latency",
            "Intel XeLowLatency",
            "Latency",
            "Low-latency mode for Intel Arc GPUs.",
            "bXeLowLatency",
            false,
        ),
        // ── Frame Generation ──
        toggle(
            "gus_dlss_fg",
            "DLSS Frame Generation",
            "Frame Generation",
            "Nvidia DLSS frame generation. Doubles framerate but adds latency.",
            "bDlssFrameGeneration",
            false,
        ),
        toggle(
            "gus_fsr_fg",
            "FSR Frame Generation",
            "Frame Generation",
            "AMD FSR frame generation. Doubles framerate but adds latency.",
            "bFSRFrameGeneration",
            false,
        ),
        toggle(
            "gus_xe_fg",
            "XeSS Frame Generation",
            "Frame Generation",
            "Intel XeSS frame generation. Doubles framerate but adds latency.",
            "bXeFrameGeneration",
            false,
        ),
        // ── Upscaling ──
        slider(
            "gus_super_sampling_quality",
            "Super Sampling Quality",
            "Upscaling",
            "Upscaler quality preset (DLSS/FSR/XeSS).",
            "SuperSamplingQuality",
            GUS_MARVEL,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_cas_sharpness",
            "CAS Sharpness",
            "Upscaling",
            "Contrast Adaptive Sharpening strength applied after upscaling.",
            "CASSharpness",
            GUS_MARVEL,
            0.0,
            1.0,
            0.05,
            0.8,
        ),
        slider(
            "gus_screen_percentage",
            "Screen Percentage",
            "Upscaling",
            "Internal render resolution percentage.",
            "ScreenPercentage",
            GUS_MARVEL,
            25.0,
            200.0,
            5.0,
            100.0,
        ),
        // ── Frame Caps ──
        slider(
            "gus_framerate_limit",
            "Frame Rate Limit",
            "Frame Caps",
            "In-game frame rate cap. 0 = uncapped.",
            "FrameRateLimit",
            GUS_MARVEL,
            0.0,
            360.0,
            5.0,
            0.0,
        ),
        slider(
            "gus_framerate_lobby",
            "Lobby Frame Rate Limit",
            "Frame Caps",
            "Frame rate cap while in lobby/menus.",
            "FrameRateLimitLobby",
            GUS_MARVEL,
            30.0,
            360.0,
            5.0,
            120.0,
        ),
        slider(
            "gus_framerate_background",
            "Background Frame Rate Limit",
            "Frame Caps",
            "Frame rate cap when the game window is unfocused.",
            "FrameRateLimitBackground",
            GUS_MARVEL,
            10.0,
            120.0,
            5.0,
            60.0,
        ),
        // ── Display ──
        slider(
            "gus_resolution_x",
            "Resolution Width",
            "Display",
            "Horizontal render resolution in pixels.",
            "ResolutionSizeX",
            GUS_MARVEL,
            640.0,
            7680.0,
            1.0,
            1920.0,
        ),
        slider(
            "gus_resolution_y",
            "Resolution Height",
            "Display",
            "Vertical render resolution in pixels.",
            "ResolutionSizeY",
            GUS_MARVEL,
            480.0,
            4320.0,
            1.0,
            1080.0,
        ),
        toggle(
            "gus_hdr",
            "HDR Output",
            "Display",
            "Enable HDR display output.",
            "bUseHDRDisplayOutput",
            false,
        ),
        slider(
            "gus_hdr_nits",
            "HDR Peak Brightness (nits)",
            "Display",
            "HDR display peak brightness target.",
            "HDRDisplayOutputNits",
            GUS_MARVEL,
            400.0,
            2000.0,
            100.0,
            1000.0,
        ),
        toggle(
            "gus_console_120fps",
            "Console 120 FPS Mode",
            "Display",
            "Forces 120 FPS mode flag (mainly relevant on console hardware).",
            "bEnableConsole120Fps",
            false,
        ),
        // ── Quality Groups ──
        slider(
            "gus_sg_viewdistance",
            "View Distance Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.ViewDistanceQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_shadow",
            "Shadow Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.ShadowQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_postprocess",
            "Post-Process Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.PostProcessQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_texture",
            "Texture Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.TextureQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_effects",
            "Effects Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.EffectsQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_foliage",
            "Foliage Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.FoliageQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_shading",
            "Shading Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.ShadingQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_reflection",
            "Reflection Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.ReflectionQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
        slider(
            "gus_sg_globalillumination",
            "Global Illumination Quality",
            "Quality Groups",
            "0 = Low, 1 = Medium, 2 = High, 3 = Epic.",
            "sg.GlobalIlluminationQuality",
            GUS_SCALABILITY,
            0.0,
            3.0,
            1.0,
            2.0,
        ),
    ]
}

#[cfg(test)]
mod validator_tests {
    //! Catalogue invariants. Failing here means a malformed entry would ship —
    //! detection or apply would silently break for that tweak in production.
    use super::*;
    use std::collections::HashSet;

    /// Extract `(key, value)` from a pattern like `r.X=0` or `+CVars=r.X=0`.
    /// Returns `(key, Some(value))` or `(key, None)` when the pattern omits a value.
    fn split_pattern(pattern: &str) -> (String, Option<String>) {
        let inner = if pattern.to_ascii_lowercase().starts_with("+cvars=") {
            &pattern["+CVars=".len()..]
        } else {
            pattern
        };
        match inner.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), Some(v.trim().to_string())),
            None => (inner.trim().to_string(), None),
        }
    }

    fn assert_id_basics(def: &TweakDefinition) {
        assert!(
            !def.id.trim().is_empty(),
            "tweak id must not be empty/whitespace"
        );
        assert!(
            !def.label.trim().is_empty(),
            "{}: label must not be empty",
            def.id
        );
        assert!(
            !def.category.trim().is_empty(),
            "{}: category must not be empty",
            def.id
        );
    }

    #[test]
    fn ids_are_unique() {
        let cat = tweak_catalogue();
        let mut seen: HashSet<&str> = HashSet::new();
        for def in &cat {
            assert!(
                seen.insert(def.id.as_str()),
                "duplicate tweak id: {}",
                def.id
            );
        }
    }

    #[test]
    fn all_have_non_empty_id_label_category() {
        for def in &tweak_catalogue() {
            assert_id_basics(def);
        }
    }

    #[test]
    fn remove_lines_invariants() {
        for def in &tweak_catalogue() {
            let TweakKind::RemoveLines { lines, .. } = &def.kind else {
                continue;
            };
            assert!(
                !lines.is_empty(),
                "{}: RemoveLines.lines must not be empty",
                def.id
            );
            for (i, line) in lines.iter().enumerate() {
                let (key, _) = split_pattern(&line.pattern);
                assert!(
                    !key.is_empty(),
                    "{}: line[{}] pattern has no key: {:?}",
                    def.id,
                    i,
                    line.pattern
                );
                if let Some(rw) = &line.replace_with {
                    let (rw_key, rw_val) = split_pattern(rw);
                    assert!(
                        !rw_key.is_empty(),
                        "{}: line[{}] replace_with has no key: {:?}",
                        def.id,
                        i,
                        rw
                    );
                    assert!(
                        rw_val.is_some(),
                        "{}: line[{}] replace_with must include a value: {:?}",
                        def.id,
                        i,
                        rw
                    );
                    assert!(
                        rw_key.eq_ignore_ascii_case(&key),
                        "{}: line[{}] replace_with key {:?} does not match pattern key {:?}",
                        def.id,
                        i,
                        rw_key,
                        key
                    );
                }
            }
        }
    }

    #[test]
    fn toggle_invariants() {
        for def in &tweak_catalogue() {
            let TweakKind::Toggle {
                key,
                on_value,
                off_value,
                ..
            } = &def.kind
            else {
                continue;
            };
            assert!(
                !key.trim().is_empty(),
                "{}: Toggle.key must not be empty",
                def.id
            );
            assert!(
                !on_value.trim().is_empty(),
                "{}: Toggle.on_value must not be empty",
                def.id
            );
            if let Some(off) = off_value {
                assert!(
                    !off.trim().is_empty(),
                    "{}: Toggle.off_value present but empty",
                    def.id
                );
                assert_ne!(
                    on_value, off,
                    "{}: Toggle.on_value == off_value (no observable toggle)",
                    def.id
                );
            }
        }
    }

    #[test]
    fn slider_invariants() {
        for def in &tweak_catalogue() {
            let TweakKind::Slider {
                key,
                min,
                max,
                step,
                default_value,
                ..
            } = &def.kind
            else {
                continue;
            };
            assert!(
                !key.trim().is_empty(),
                "{}: Slider.key must not be empty",
                def.id
            );
            assert!(
                min < max,
                "{}: Slider min ({}) must be < max ({})",
                def.id,
                min,
                max
            );
            assert!(
                *step > 0.0,
                "{}: Slider step ({}) must be > 0",
                def.id,
                step
            );
            assert!(
                default_value >= min && default_value <= max,
                "{}: Slider default ({}) outside [{}, {}]",
                def.id,
                default_value,
                min,
                max
            );
        }
    }

    #[test]
    fn batch_toggle_invariants() {
        for def in &tweak_catalogue() {
            let TweakKind::BatchToggle { entries, .. } = &def.kind else {
                continue;
            };
            assert!(
                !entries.is_empty(),
                "{}: BatchToggle.entries must not be empty",
                def.id
            );
            // Within one BatchToggle, keys should be unique to avoid edits
            // overwriting each other in the apply queue.
            let mut seen: HashSet<String> = HashSet::new();
            for (i, entry) in entries.iter().enumerate() {
                assert!(
                    !entry.key.trim().is_empty(),
                    "{}: entry[{}] key must not be empty",
                    def.id,
                    i
                );
                assert!(
                    !entry.on_value.trim().is_empty(),
                    "{}: entry[{}] on_value must not be empty",
                    def.id,
                    i
                );
                if let Some(off) = &entry.off_value {
                    assert_ne!(
                        &entry.on_value, off,
                        "{}: entry[{}] on_value == off_value",
                        def.id, i
                    );
                }
                let key_lower = entry.key.to_ascii_lowercase();
                assert!(
                    seen.insert(key_lower),
                    "{}: BatchToggle has duplicate key {:?}",
                    def.id,
                    entry.key
                );
            }
        }
    }

    #[test]
    fn pak_only_tweaks_dont_use_scalability_section() {
        // pak_only: true means the tweak only edits pak INI files. Putting a
        // scalability_section on it would suggest Scalability.ini editing, which
        // contradicts the pak_only flag.
        for def in &tweak_catalogue() {
            if !def.pak_only {
                continue;
            }
            match &def.kind {
                TweakKind::Toggle {
                    scalability_section: Some(s),
                    ..
                }
                | TweakKind::Slider {
                    scalability_section: Some(s),
                    ..
                } => panic!(
                    "{}: pak_only tweak should not have scalability_section ({:?})",
                    def.id, s
                ),
                TweakKind::RemoveLines { lines, .. } => {
                    for (i, line) in lines.iter().enumerate() {
                        if let Some(s) = &line.scalability_section {
                            panic!(
                                "{}: pak_only tweak line[{}] has scalability_section ({:?})",
                                def.id, i, s
                            );
                        }
                    }
                }
                TweakKind::BatchToggle { entries, .. } => {
                    for (i, entry) in entries.iter().enumerate() {
                        if let Some(s) = &entry.scalability_section {
                            panic!(
                                "{}: pak_only tweak entry[{}] has scalability_section ({:?})",
                                def.id, i, s
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
