use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct LauncherEntry {
    pub app_id: String,
    pub name: String,
    pub command: String,
    pub icon: Option<String>,
}

#[derive(Debug)]
pub struct Launcher {
    pub applications: Vec<LauncherEntry>,
    pub filter: String,
}

#[derive(Debug, Clone)]
pub enum DesktopEntryType {
    Application,
    Link,
    Directory,
}

#[derive(Debug, Clone)]
pub struct DesktopFile {
    pub entry_type: DesktopEntryType,
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    pub no_display: bool,
    pub terminal: bool,
}

/// Parse a single .desktop file and return its metadata.
/// Returns None if the file is not a valid Application entry.
pub fn parse_desktop_file(path: &Path) -> Option<DesktopFile> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut entry_type = DesktopEntryType::Application;
    let mut name = String::new();
    let mut exec = String::new();
    let mut icon = None;
    let mut categories = Vec::new();
    let mut no_display = false;
    let mut terminal = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "Type" => {
                    entry_type = match value {
                        "Application" => DesktopEntryType::Application,
                        "Link" => DesktopEntryType::Link,
                        _ => DesktopEntryType::Directory,
                    };
                }
                "Name" if name.is_empty() => name = value.to_string(),
                "Exec" => exec = value.to_string(),
                "Icon" => icon = Some(value.to_string()),
                "Categories" => {
                    categories = value.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                }
                "NoDisplay" => no_display = value == "true",
                "Terminal" => terminal = value == "true",
                _ => {}
            }
        }
    }

    if !matches!(entry_type, DesktopEntryType::Application) || name.is_empty() || exec.is_empty() {
        return None;
    }

    Some(DesktopFile {
        entry_type,
        name,
        exec,
        icon,
        categories,
        no_display,
        terminal,
    })
}

/// Discover .desktop files from standard XDG paths.
pub fn discover_desktop_files() -> Vec<DesktopFile> {
    let mut files = Vec::new();
    let mut paths = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".local/share/applications"));
    }

    for dir in &paths {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                    if let Some(df) = parse_desktop_file(&path) {
                        if !df.no_display {
                            files.push(df);
                        }
                    }
                }
            }
        }
    }

    files
}

impl Launcher {
    pub fn new() -> Self {
        Launcher {
            applications: Vec::new(),
            filter: String::new(),
        }
    }

    pub fn discover(&mut self) {
        let files = discover_desktop_files();
        self.applications = files
            .into_iter()
            .map(|df| {
                let app_id = df
                    .exec
                    .split_whitespace()
                    .next()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                LauncherEntry {
                    app_id: app_id.clone(),
                    name: df.name,
                    command: df.exec,
                    icon: df.icon,
                }
            })
            .collect();
    }

    /// Filter the visible entries (unused until the shell grows a
    /// launcher search field — J4 follow-up).
    #[allow(dead_code)]
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_lowercase();
    }

    pub fn filtered(&self) -> Vec<&LauncherEntry> {
        if self.filter.is_empty() {
            return self.applications.iter().collect();
        }
        self.applications
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&self.filter)
                    || e.app_id.to_lowercase().contains(&self.filter)
            })
            .collect()
    }

    pub fn launch(&self, index: usize) -> Option<std::process::Child> {
        let entry = self.applications.get(index)?;
        let command = entry.command.replace("%f", "").replace("%F", "")
            .replace("%u", "").replace("%U", "");
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }
        std::process::Command::new(parts[0])
            .args(&parts[1..])
            .env("WAYLAND_DISPLAY", std::env::var("WAYLAND_DISPLAY").unwrap_or_default())
            .spawn()
            .ok()
    }
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_test_desktop(dir: &Path, content: &str) -> PathBuf {
        let path = dir.join("test.desktop");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "{}", content).unwrap();
        path
    }

    #[test]
    fn parse_valid_desktop_file() {
        let tmp = std::env::temp_dir().join("veyra-test-desktop");
        let _ = std::fs::create_dir_all(&tmp);
        let path = create_test_desktop(&tmp,
            "[Desktop Entry]\nType=Application\nName=Test App\nExec=test-app --flag\nIcon=test-icon\nCategories=Utility;\n"
        );
        let df = parse_desktop_file(&path).unwrap();
        assert_eq!(df.name, "Test App");
        assert_eq!(df.exec, "test-app --flag");
        assert_eq!(df.icon.unwrap(), "test-icon");
        assert!(df.categories.contains(&"Utility".to_string()));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_desktop_no_display() {
        let tmp = std::env::temp_dir().join("veyra-test-desktop-nd");
        let _ = std::fs::create_dir_all(&tmp);
        let path = create_test_desktop(&tmp,
            "[Desktop Entry]\nType=Application\nName=Hidden App\nExec=hidden\nNoDisplay=true\n"
        );
        let df = parse_desktop_file(&path).unwrap();
        assert!(df.no_display);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_invalid_desktop_missing_name() {
        let tmp = std::env::temp_dir().join("veyra-test-desktop-mn");
        let _ = std::fs::create_dir_all(&tmp);
        let path = create_test_desktop(&tmp,
            "[Desktop Entry]\nType=Application\nExec=test\n"
        );
        assert!(parse_desktop_file(&path).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_invalid_desktop_wrong_type() {
        let tmp = std::env::temp_dir().join("veyra-test-desktop-wt");
        let _ = std::fs::create_dir_all(&tmp);
        let path = create_test_desktop(&tmp,
            "[Desktop Entry]\nType=Link\nName=Link\nURL=http://example.com\n"
        );
        assert!(parse_desktop_file(&path).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn launcher_discover_and_filter() {
        let mut launcher = Launcher::new();
        launcher.discover();
        launcher.set_filter("nonexistent");
        let results = launcher.filtered();
        assert!(results.is_empty());
    }

    #[test]
    fn launcher_filter_by_name() {
        let mut launcher = Launcher::new();
        launcher.applications.push(LauncherEntry {
            app_id: "firefox".into(),
            name: "Firefox Web Browser".into(),
            command: "firefox".into(),
            icon: None,
        });
        launcher.applications.push(LauncherEntry {
            app_id: "code".into(),
            name: "Visual Studio Code".into(),
            command: "code".into(),
            icon: None,
        });

        launcher.set_filter("fire");
        let results = launcher.filtered();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Firefox Web Browser");

        launcher.set_filter("code");
        let results = launcher.filtered();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Visual Studio Code");
    }

    #[test]
    fn launcher_empty_filter_returns_all() {
        let mut launcher = Launcher::new();
        launcher.applications.push(LauncherEntry {
            app_id: "a".into(), name: "A".into(), command: "a".into(), icon: None,
        });
        launcher.applications.push(LauncherEntry {
            app_id: "b".into(), name: "B".into(), command: "b".into(), icon: None,
        });
        assert_eq!(launcher.filtered().len(), 2);
    }

    #[test]
    fn launch_command_construction() {
        let mut launcher = Launcher::new();
        launcher.applications.push(LauncherEntry {
            app_id: "test".into(),
            name: "Test".into(),
            command: "test-app --flag %f".into(),
            icon: None,
        });
        let result = launcher.launch(0);
        // launch returns Option; may be None if command doesn't exist
        // but should not crash
        let _ = result;
    }
}
