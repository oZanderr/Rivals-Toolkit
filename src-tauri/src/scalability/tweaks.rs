use serde::{Deserialize, Serialize};

fn default_cv_section() -> String {
    "ConsoleVariables".to_string()
}

/// A single pattern for a `RemoveLines` tweak, paired with the scalability
/// section it must live under to be effective.
/// For pak/engine INI writes the section is ignored — CVars go flat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScalabilityLine {
    pub pattern: String,
    /// Scalability-file section bracket, e.g. `"PostProcessQuality@0"`.
    /// Ignored when writing to DeviceProfiles or DefaultEngine.ini.
    #[serde(default = "default_cv_section")]
    pub section: String,
    /// When `true`, this line is only written when the context is a pak mod INI.
    /// It is never added to a scalability file.
    #[serde(default)]
    pub pak_only: bool,
}

/// Describes what kind of tweak this is and how to apply/detect it.
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
        /// What `active` should report when the key is absent from the INI
        /// (i.e. the engine default). `true` = feature is ON by default
        /// (e.g. CAS Sharpening, Font AA); `false` = OFF.
        #[serde(default)]
        default_enabled: bool,
        /// Scalability-file section new keys are inserted under.
        /// Ignored when writing to DeviceProfiles or DefaultEngine.ini.
        #[serde(default = "default_cv_section")]
        section: String,
    },
    Slider {
        key: String,
        min: f64,
        max: f64,
        step: f64,
        default_value: f64,
        /// Scalability-file section new keys are inserted under.
        /// Ignored when writing to DeviceProfiles or DefaultEngine.ini.
        #[serde(default = "default_cv_section")]
        section: String,
    },
}

/// A named, categorized tweak the user can toggle or adjust.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakDefinition {
    pub id: String,
    pub label: String,
    pub category: String,
    pub description: String,
    #[serde(default)]
    pub pak_only: bool,
    /// For pak-only tweaks that live in a non-`[ConsoleVariables]` section of
    /// DefaultEngine.ini (e.g. `"Script/Engine.UserInterfaceSettings"`).
    /// `None` means the standard `[ConsoleVariables]` section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
    #[serde(flatten)]
    pub kind: TweakKind,
}

/// Current state of a tweak detected from INI content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakState {
    pub id: String,
    pub active: bool,
    pub current_value: Option<String>,
}

/// User-provided setting to apply for a specific tweak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TweakSetting {
    pub id: String,
    pub enabled: bool,
    pub value: Option<String>,
}

/// Build the full catalogue of available tweaks.
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
                           bodies clipping through the floor. Only effective as a pak mod."
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
                           by re-enabling Niagara sprite rendering. Only effective as a pak mod."
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
                           have black hair."
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
        // Display
        TweakDefinition {
            id: "application_scale".into(),
            label: "Application Scale".into(),
            category: "Display".into(),
            description: "Controls UI / viewport scaling. Default is 1.0. Values above 1.0 \
                           zoom in (useful for 4:3 without black bars). \
                           Only effective as a pak mod (DefaultEngine.ini)."
                .into(),
            pak_only: true,
            engine_section: Some("/Script/Engine.UserInterfaceSettings".into()),
            kind: TweakKind::Slider {
                key: "ApplicationScale".into(),
                min: 0.5,
                max: 2.0,
                step: 0.05,
                default_value: 1.0,
                section: "ConsoleVariables".into(), // pak-only; section unused for engine writes
            },
        },
        TweakDefinition {
            id: "font_aa".into(),
            label: "Font Anti-Aliasing".into(),
            category: "Display".into(),
            description: "Toggles anti-aliasing on UI text / fonts. Only effective as a pak mod.".into(),
            pak_only: true,
            engine_section: None,
            kind: TweakKind::Toggle {
                key: "Slate.EnableFontAntiAliasing".into(),
                on_value: "1".into(),
                off_value: "0".into(),
                default_enabled: true,
                section: "ConsoleVariables".into(), // pak-only; section unused for engine writes
            },
        },
    ]
}
