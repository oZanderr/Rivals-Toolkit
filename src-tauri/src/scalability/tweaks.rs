use serde::{Deserialize, Serialize};

fn default_cv_section() -> String {
    "ConsoleVariables".to_string()
}

/// One line pattern used by a `RemoveLines` tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScalabilityLine {
    pub pattern: String,
    /// Target scalability section for this line.
    #[serde(default = "default_cv_section")]
    pub section: String,
    /// If true, only applied in pak INI context.
    #[serde(default)]
    pub pak_only: bool,
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
        off_value: String,
        /// Active-state fallback when the key is absent.
        #[serde(default)]
        default_enabled: bool,
        /// Section used when inserting new keys into scalability files.
        #[serde(default = "default_cv_section")]
        section: String,
    },
    Slider {
        key: String,
        min: f64,
        max: f64,
        step: f64,
        default_value: f64,
        /// Section used when inserting new keys into scalability files.
        #[serde(default = "default_cv_section")]
        section: String,
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
    /// Optional Engine.ini section for pak-only tweaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.PostProcessing.DisableMaterials=1".into(), section: "PostProcessQuality@0".into(), pak_only: false },
                    ScalabilityLine { pattern: "r.CustomDepth=0".into(),                     section: "ConsoleVariables".into(),    pak_only: true  },
                    ScalabilityLine { pattern: "r.LightTile.Enable=0".into(),                section: "ConsoleVariables".into(),    pak_only: true  },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "m.Portal.ScreenPercentageLowerLimit=1".into(), section: "EffectsQuality@0".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "p.SimCollisionEnabled=0".into(), section: "ConsoleVariables".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "fx.EnableNiagaraSpriteRendering=0".into(), section: "ConsoleVariables".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.LightMaxDrawDistanceScale=0.00000001".into(), section: "ShadowQuality@0".into(),             pak_only: false },
                    ScalabilityLine { pattern: "r.LightFadeDistance=1".into(),                  section: "GlobalIlluminationQuality@0".into(), pak_only: false },
                    ScalabilityLine { pattern: "r.LightCullingDistance=1".into(),               section: "GlobalIlluminationQuality@0".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::Slider {
                key: "r.TonemapperGamma".into(),
                min: 0.0,
                max: 5.0,
                step: 0.1,
                default_value: 2.2,
                section: "PostProcessQuality@0".into(),
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.SceneColorFormat=0".into(), section: "EffectsQuality@0".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.PostProcessing.EnableEyeAdaptation=0".into(), section: "PostProcessQuality@0".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.SeparateTranslucency=0".into(), section: "PostProcessQuality@0".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::Toggle {
                key: "r.PostProcessing.EnableCAS".into(),
                on_value: "1".into(),
                off_value: "0".into(),
                default_enabled: true,
                section: "PostProcessQuality@0".into(),
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.AnisotropicMaterials=0".into(),    section: "ShadingQuality@0".into(),   pak_only: false },
                    ScalabilityLine { pattern: "r.VT.AnisotropicMaterials=0".into(), section: "TextureQuality@0".into(),   pak_only: false },
                    ScalabilityLine { pattern: "r.VT.MaxAnisotropy=0".into(),        section: "TextureQuality@0".into(),   pak_only: false },
                    ScalabilityLine { pattern: "r.MaxAnisotropy=0".into(),           section: "TextureQuality@0".into(),   pak_only: false },
                ],
            },
        },
        TweakDefinition {
            id: "fix_mipmap_bias".into(),
            label: "Fix MipMap LOD Bias".into(),
            category: "Sharpness & Textures".into(),
            description: "Removes high MipMap LOD bias that degrades texture quality.".into(),
            pak_only: false,
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.MipMapLODBias=15".into(), section: "TextureQuality@0".into(), pak_only: false },
                ],
            },
        },
        TweakDefinition {
            id: "fix_streaming_bias".into(),
            label: "Fix Streaming Mip Bias".into(),
            category: "Sharpness & Textures".into(),
            description: "Removes streaming mip bias that can cause blurry textures.".into(),
            pak_only: false,
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.Streaming.MipBias=2".into(), section: "TextureQuality@0".into(), pak_only: false },
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
            engine_section: None,
            kind: TweakKind::RemoveLines {
                lines: vec![
                    ScalabilityLine { pattern: "r.Streaming.PoolSize=1".into(), section: "TextureQuality@0".into(), pak_only: false },
                ],
            },
        },
        // Latency
        TweakDefinition {
            id: "latency_gt_sync_type".into(),
            label: "Game Thread Sync Type".into(),
            category: "Latency".into(),
            description: "Controls game-thread sync target. 0 = render thread, \
                           1 = RHI thread, 2 = GPU swap-chain flip."
                .into(),
            pak_only: true,
            engine_section: None,
            kind: TweakKind::Slider {
                key: "r.GTSyncType".into(),
                min: 0.0,
                max: 2.0,
                step: 1.0,
                default_value: 1.0,
                section: "ConsoleVariables".into(),
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
            engine_section: None,
            kind: TweakKind::Toggle {
                key: "r.FinishCurrentFrame".into(),
                on_value: "1".into(),
                off_value: "0".into(),
                default_enabled: false,
                section: "ConsoleVariables".into(),
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
            engine_section: None,
            kind: TweakKind::Toggle {
                key: "r.OneFrameThreadLag".into(),
                on_value: "1".into(),
                off_value: "0".into(),
                default_enabled: true,
                section: "ConsoleVariables".into(),
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
            engine_section: None,
            kind: TweakKind::Slider {
                key: "rhi.SyncInterval".into(),
                min: 0.0,
                max: 4.0,
                step: 1.0,
                default_value: 0.0,
                section: "ConsoleVariables".into(),
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
            engine_section: Some("/Script/Engine.UserInterfaceSettings".into()),
            kind: TweakKind::Slider {
                key: "ApplicationScale".into(),
                min: 0.5,
                max: 2.0,
                step: 0.05,
                default_value: 1.0,
                section: "ConsoleVariables".into(),
            },
        },
        TweakDefinition {
            id: "font_aa".into(),
            label: "Font Anti-Aliasing".into(),
            category: "Display".into(),
            description: "Toggles anti-aliasing on UI text / fonts. Not much performance change, mostly stylistic preference.".into(),
            pak_only: true,
            engine_section: None,
            kind: TweakKind::Toggle {
                key: "Slate.EnableFontAntiAliasing".into(),
                on_value: "1".into(),
                off_value: "0".into(),
                default_enabled: true,
                section: "ConsoleVariables".into(),
            },
        },
        TweakDefinition {
            id: "hide_marvel_widget_ui".into(),
            label: "Hide Overhead HP/Name UI".into(),
            category: "Display".into(),
            description: "Hides the UI for overhead health bars and player names. Potentially helpful for those on low-end hardware."
                .into(),
            pak_only: true,
            engine_section: None,
            kind: TweakKind::Toggle {
                key: "UI.HideMarvelWidgetUI".into(),
                on_value: "1".into(),
                off_value: "0".into(),
                default_enabled: false,
                section: "ConsoleVariables".into(),
            },
        },
    ]
}
