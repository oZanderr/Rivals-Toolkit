use serde::{Deserialize, Serialize};

/// One line pattern used by a `RemoveLines` tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScalabilityLine {
    pub pattern: String,
    /// Target scalability section (e.g. `PostProcessQuality@0`).
    /// `None` for lines that should only be removed, never re-added to scalability files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
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
        lines: Vec<ScalabilityLine>,
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
                    ScalabilityLine { pattern: "r.PostProcessing.DisableMaterials=1".into(), section: Some("PostProcessQuality@0".into()) },
                    ScalabilityLine { pattern: "r.CustomDepth=0".into(),                     section: None },
                    ScalabilityLine { pattern: "r.LightTile.Enable=0".into(),                section: None },
                ],
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
                    ScalabilityLine { pattern: "m.Portal.ScreenPercentageLowerLimit=1".into(), section: Some("EffectsQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "p.SimCollisionEnabled=0".into(), section: None },
                ],
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
                    ScalabilityLine { pattern: "fx.EnableNiagaraSpriteRendering=0".into(), section: None },
                ],
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
                    ScalabilityLine { pattern: "r.LightMaxDrawDistanceScale=0.00000001".into(), section: Some("ShadowQuality@0".into()) },
                    ScalabilityLine { pattern: "r.LightFadeDistance=1".into(),                  section: Some("GlobalIlluminationQuality@0".into()) },
                    ScalabilityLine { pattern: "r.LightCullingDistance=1".into(),               section: Some("GlobalIlluminationQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "r.SceneColorFormat=0".into(), section: Some("EffectsQuality@0".into()) },
                ],
            },
        },
        TweakDefinition {
            id: "fix_black_characters".into(),
            label: "Fix Incorrect Black Characters".into(),
            category: "Lighting & Color".into(),
            description: "Removes the eye adaptation disable that causes characters to render \
                           incorrectly dark."
                .into(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.PostProcessing.EnableEyeAdaptation=0".into(), section: Some("PostProcessQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "r.SeparateTranslucency=0".into(), section: Some("PostProcessQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "r.AnisotropicMaterials=0".into(),    section: Some("ShadingQuality@0".into()) },
                    ScalabilityLine { pattern: "r.VT.AnisotropicMaterials=0".into(), section: Some("TextureQuality@0".into()) },
                    ScalabilityLine { pattern: "r.VT.MaxAnisotropy=0".into(),        section: Some("TextureQuality@0".into()) },
                    ScalabilityLine { pattern: "r.MaxAnisotropy=0".into(),           section: Some("TextureQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "r.MipMapLODBias=15".into(), section: Some("TextureQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "r.Streaming.MipBias=2".into(), section: Some("TextureQuality@0".into()) },
                ],
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
                    ScalabilityLine { pattern: "r.Streaming.PoolSize=1".into(), section: Some("TextureQuality@0".into()) },
                ],
            },
        },
        //Experimental
        TweakDefinition {
            id: "black_backgrounds".into(),
            label: "Black Backgrounds".into(),
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
        // Latency
        TweakDefinition {
            id: "latency_reflex_mode".into(),
            label: "NVIDIA Reflex Mode".into(),
            category: "Latency".into(),
            description: "Controls NVIDIA Reflex mode via Streamline. \
                           0 = Off, 1 = On (Low Latency), 2 = On + Boost. \
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
                scalability_section: None,
                engine_section: None,
            },
        },
        TweakDefinition {
            id: "latency_gt_sync_type".into(),
            label: "Game Thread Sync Type".into(),
            category: "Latency".into(),
            description: "Controls game-thread sync target. 0 = render thread, \
                           1 = RHI thread, 2 = GPU swap-chain flip."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "r.GTSyncType".into(),
                min: 0.0,
                max: 2.0,
                step: 1.0,
                default_value: 1.0,
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
                           throughput and overall performance."
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
                           possible performance cost."
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
                           immediately (unlocked), 1 = every vblank, 2 = every 2 vblanks, \
                           etc. Higher values generally increase latency and lower frame rate."
                .into(),
            pak_only: true,
            kind: TweakKind::Slider {
                key: "rhi.SyncInterval".into(),
                min: 0.0,
                max: 4.0,
                step: 1.0,
                default_value: 0.0,
                scalability_section: None,
                engine_section: None,
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
                           team and enemy highlights. Disable to remove all outline/silhouette effects."
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
    ]
}
