//! Strongly-typed configuration system.
//!
//! Config is loaded once at startup. Defaults are hardcoded.
//! Values from `~/.config/veyra/config.toml` override defaults.
//! No live reload — configuration changes require a restart.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    #[serde(default = "default_focus_key")]
    pub focus_key: String,
    #[serde(default = "default_overview_key")]
    pub overview_key: String,
    #[serde(default = "default_sensitivity")]
    pub sensitivity: f32,
    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: f32,
}

fn default_focus_key() -> String { "F6".into() }
fn default_overview_key() -> String { "F9".into() }
fn default_sensitivity() -> f32 { 1.0 }
fn default_scroll_speed() -> f32 { 1.0 }

impl Default for InputConfig {
    fn default() -> Self {
        InputConfig {
            focus_key: default_focus_key(),
            overview_key: default_overview_key(),
            sensitivity: default_sensitivity(),
            scroll_speed: default_scroll_speed(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    #[serde(default = "default_focus_distance")]
    pub focus_distance: f32,
    #[serde(default = "default_transition_ms")]
    pub transition_ms: u64,
    #[serde(default = "default_yaw")]
    pub default_yaw: f32,
    #[serde(default = "default_pitch")]
    pub default_pitch: f32,
    #[serde(default = "default_distance")]
    pub default_distance: f32,
}

fn default_focus_distance() -> f32 { 500.0 }
fn default_transition_ms() -> u64 { 300 }
fn default_yaw() -> f32 { 0.0 }
fn default_pitch() -> f32 { 0.0 }
fn default_distance() -> f32 { 800.0 }

impl Default for CameraConfig {
    fn default() -> Self {
        CameraConfig {
            focus_distance: default_focus_distance(),
            transition_ms: default_transition_ms(),
            default_yaw: default_yaw(),
            default_pitch: default_pitch(),
            default_distance: default_distance(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace_count")]
    pub count: usize,
    #[serde(default = "default_layout")]
    pub default_layout: String,
}

fn default_workspace_count() -> usize { 3 }
fn default_layout() -> String { "freeform".into() }

impl Default for WorkspaceConfig {
    fn default() -> Self {
        WorkspaceConfig {
            count: default_workspace_count(),
            default_layout: default_layout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutConfig {
    #[serde(default = "default_spacing")]
    pub spacing: f32,
    #[serde(default = "default_margin")]
    pub margin: f32,
}

fn default_spacing() -> f32 { 40.0 }
fn default_margin() -> f32 { 100.0 }

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            spacing: default_spacing(),
            margin: default_margin(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    #[serde(default = "default_bg_color")]
    pub background_color: [f32; 3],
}

fn default_bg_color() -> [f32; 3] { [0.15, 0.15, 0.15] }

impl Default for AppearanceConfig {
    fn default() -> Self {
        AppearanceConfig {
            background_color: default_bg_color(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortcutConfig {
    #[serde(default = "default_alt_tab")]
    pub alt_tab: String,
    #[serde(default = "default_launcher")]
    pub launcher: String,
    #[serde(default = "default_toggle_shelf")]
    pub toggle_shelf: String,
    #[serde(default = "default_send_to_shelf")]
    pub send_to_shelf: String,
    #[serde(default = "default_reset_camera")]
    pub reset_camera: String,
}

fn default_alt_tab() -> String { "Alt+Tab".into() }
fn default_launcher() -> String { "Meta+Space".into() }
fn default_toggle_shelf() -> String { "Meta+D".into() }
fn default_send_to_shelf() -> String { "Meta+Down".into() }
fn default_reset_camera() -> String { "Escape".into() }

impl Default for ShortcutConfig {
    fn default() -> Self {
        ShortcutConfig {
            alt_tab: default_alt_tab(),
            launcher: default_launcher(),
            toggle_shelf: default_toggle_shelf(),
            send_to_shelf: default_send_to_shelf(),
            reset_camera: default_reset_camera(),
        }
    }
}

/// The top-level configuration. Every field has a default value.
/// File-based overrides are merged on top of defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub input: InputConfig,
    #[serde(default)]
    pub camera: CameraConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub shortcuts: ShortcutConfig,
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 { CONFIG_VERSION }

impl Default for Config {
    fn default() -> Self {
        Config {
            input: InputConfig::default(),
            camera: CameraConfig::default(),
            workspace: WorkspaceConfig::default(),
            layout: LayoutConfig::default(),
            appearance: AppearanceConfig::default(),
            shortcuts: ShortcutConfig::default(),
            version: CONFIG_VERSION,
        }
    }
}

impl Config {
    /// Load configuration from disk with fallback to defaults.
    ///
    /// Precedence:
    /// 1. Built-in defaults (Config::default())
    /// 2. `$XDG_CONFIG_HOME/veyra/config.toml` (or `~/.config/veyra/config.toml`)
    ///
    /// Missing file → use defaults (no error).
    /// Invalid file → log warning, use defaults.
    /// Out-of-range values → clamped.
    pub fn load() -> Self {
        let mut config = Config::default();

        let path = config_path();
        if !path.exists() {
            return config;
        }

        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) => {
                warn!(?e, path = %path.display(), "could not read config file, using defaults");
                return config;
            }
        };

        match toml::from_str::<ConfigFile>(&data) {
            Ok(file_config) => {
                config.apply_overrides(file_config);
            }
            Err(e) => {
                warn!(?e, path = %path.display(), "invalid config file, using defaults");
            }
        }

        config.validate();

        config
    }

    fn apply_overrides(&mut self, overrides: ConfigFile) {
        if let Some(input) = overrides.input {
            self.input.focus_key = input.focus_key;
            self.input.overview_key = input.overview_key;
            self.input.sensitivity = input.sensitivity;
            self.input.scroll_speed = input.scroll_speed;
        }
        if let Some(camera) = overrides.camera {
            self.camera.focus_distance = camera.focus_distance;
            self.camera.transition_ms = camera.transition_ms;
            self.camera.default_yaw = camera.default_yaw;
            self.camera.default_pitch = camera.default_pitch;
            self.camera.default_distance = camera.default_distance;
        }
        if let Some(workspace) = overrides.workspace {
            self.workspace.count = workspace.count;
            self.workspace.default_layout = workspace.default_layout;
        }
        if let Some(layout) = overrides.layout {
            self.layout.spacing = layout.spacing;
            self.layout.margin = layout.margin;
        }
        if let Some(appearance) = overrides.appearance {
            self.appearance.background_color = appearance.background_color;
        }
        if let Some(shortcuts) = overrides.shortcuts {
            self.shortcuts.alt_tab = shortcuts.alt_tab;
            self.shortcuts.launcher = shortcuts.launcher;
            self.shortcuts.toggle_shelf = shortcuts.toggle_shelf;
            self.shortcuts.send_to_shelf = shortcuts.send_to_shelf;
            self.shortcuts.reset_camera = shortcuts.reset_camera;
        }
        if let Some(v) = overrides.version {
            self.version = v;
        }
    }

    fn validate(&mut self) {
        if self.workspace.count < 1 {
            warn!("workspace.count {} clamped to 1", self.workspace.count);
            self.workspace.count = 1;
        }
        if self.layout.spacing < 0.0 {
            warn!("layout.spacing {} clamped to 0.0", self.layout.spacing);
            self.layout.spacing = 0.0;
        }
        if self.layout.margin < 0.0 {
            warn!("layout.margin {} clamped to 0.0", self.layout.margin);
            self.layout.margin = 0.0;
        }
        if self.input.sensitivity <= 0.0 {
            warn!("input.sensitivity {} clamped to 0.1", self.input.sensitivity);
            self.input.sensitivity = 0.1;
        }
        if self.camera.transition_ms > 5000 {
            warn!("camera.transition_ms {} clamped to 5000", self.camera.transition_ms);
            self.camera.transition_ms = 5000;
        }
    }
}

/// The file-level config (all fields optional for partial overrides).
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    input: Option<InputConfig>,
    #[serde(default)]
    camera: Option<CameraConfig>,
    #[serde(default)]
    workspace: Option<WorkspaceConfig>,
    #[serde(default)]
    layout: Option<LayoutConfig>,
    #[serde(default)]
    appearance: Option<AppearanceConfig>,
    #[serde(default)]
    shortcuts: Option<ShortcutConfig>,
    #[serde(default)]
    version: Option<u32>,
}

/// Get the path to the config file.
/// Uses `VEYRA_CONFIG_PATH` env var for testing, otherwise standard path.
fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("VEYRA_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".config")
        });
    base.join("veyra").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Run a test with a temporary config file set via VEYRA_CONFIG_PATH.
    /// Uses a unique temp dir per call so parallel tests don't interfere.
    fn with_config(toml_content: &str) -> Config {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("veyra_config_test_{}", id));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        let mut file = fs::File::create(&path).unwrap();
        write!(file, "{}", toml_content).unwrap();

        std::env::set_var("VEYRA_CONFIG_PATH", &path);
        let config = Config::load();
        std::env::remove_var("VEYRA_CONFIG_PATH");
        let _ = fs::remove_dir_all(&dir);
        config
    }

    #[test]
    fn default_config_has_sensible_values() {
        let config = Config::default();
        assert_eq!(config.workspace.count, 3);
        assert_eq!(config.input.sensitivity, 1.0);
        assert_eq!(config.camera.default_distance, 800.0);
        assert_eq!(config.layout.spacing, 40.0);
        assert_eq!(config.appearance.background_color, [0.15, 0.15, 0.15]);
        assert_eq!(config.version, 1);
    }

    #[test]
    fn override_single_value_via_toml() {
        let toml = r#"
[workspace]
count = 5
"#;
        let config = with_config(toml);
        assert_eq!(config.workspace.count, 5);
        assert_eq!(config.camera.default_distance, 800.0);
        assert_eq!(config.input.sensitivity, 1.0);
    }

    #[test]
    fn partial_config_missing_sections_use_defaults() {
        let toml = r#"
[input]
sensitivity = 2.0
"#;
        let config = with_config(toml);
        assert_eq!(config.input.sensitivity, 2.0);
        assert_eq!(config.workspace.count, 3);
        assert_eq!(config.camera.focus_distance, 500.0);
    }

    #[test]
    fn invalid_toml_uses_defaults() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("veyra_config_test_invalid_{}", id));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test_invalid.toml");
        fs::write(&path, "not valid toml {{{").unwrap();

        std::env::set_var("VEYRA_CONFIG_PATH", &path);
        let config = Config::load();
        std::env::remove_var("VEYRA_CONFIG_PATH");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(config.workspace.count, 3);
    }

    #[test]
    fn workspace_count_zero_clamped_to_one() {
        let config = with_config("[workspace]\ncount = 0\n");
        assert_eq!(config.workspace.count, 1);
    }

    #[test]
    fn negative_spacing_clamped_to_zero() {
        let config = with_config("[layout]\nspacing = -10.0\n");
        assert_eq!(config.layout.spacing, 0.0);
    }

    #[test]
    fn unknown_fields_ignored() {
        let toml = r#"
[input]
sensitivity = 1.5
unknown_field = "ignored"

[extra_section]
foo = "bar"
"#;
        let config = with_config(toml);
        assert!((config.input.sensitivity - 1.5).abs() < 0.001);
        assert_eq!(config.workspace.count, 3);
    }

    #[test]
    fn toml_parse_workspace_override_direct() {
        let toml = "[workspace]\ncount = 5\n";
        let cf: ConfigFile = toml::from_str(toml).unwrap();
        assert!(cf.workspace.is_some());
        assert_eq!(cf.workspace.unwrap().count, 5);
    }

    #[test]
    fn serialization_round_trip() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.workspace.count, 3);
        assert_eq!(parsed.input.sensitivity, 1.0);
    }

    #[test]
    fn missing_config_file_uses_defaults() {
        std::env::set_var("VEYRA_CONFIG_PATH", "/nonexistent/path/config.toml");
        let config = Config::load();
        std::env::remove_var("VEYRA_CONFIG_PATH");
        assert_eq!(config.workspace.count, 3);
    }
}
