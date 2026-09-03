//! Application registration (J1).
//!
//! Since the focus/MRU work, application switching lives in
//! `crate::focus_history::FocusHistory` (per-window MRU, updated on
//! actual focus transitions). This module is the app-id bookkeeping
//! the compositor keeps for shell queries (which applications exist).

use crate::scene::VisualId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppId(pub String);

impl AppId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct ApplicationSwitcher {
    apps: Vec<AppId>,
}

impl ApplicationSwitcher {
    pub fn new() -> Self {
        ApplicationSwitcher {
            apps: Vec::new(),
        }
    }

    pub fn register_visual(&mut self, app_id: &str, _visual_id: VisualId) {
        let id = AppId(app_id.to_string());
        if !self.apps.contains(&id) {
            self.apps.push(id);
        }
    }

    pub fn unregister_visual(&mut self, _app_id: &str, _visual_id: VisualId) {
        // Deliberately no-op: the app-id list is a registry of
        // applications seen this session, not a window set (window
        // state lives in toplevels + focus history).
    }

    pub fn all_app_ids(&self) -> &[AppId] {
        &self.apps
    }
}

impl Default for ApplicationSwitcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_single_app() {
        let mut sw = ApplicationSwitcher::new();
        sw.register_visual("org.wezfurlong.wezterm", VisualId(1));
        assert_eq!(sw.all_app_ids().len(), 1);
        assert_eq!(sw.all_app_ids()[0].as_str(), "org.wezfurlong.wezterm");
    }

    #[test]
    fn register_multiple_visuals_same_app() {
        let mut sw = ApplicationSwitcher::new();
        sw.register_visual("org.wezfurlong.wezterm", VisualId(1));
        sw.register_visual("org.wezfurlong.wezterm", VisualId(2));
        assert_eq!(sw.all_app_ids().len(), 1);
    }

    #[test]
    fn two_apps_registered() {
        let mut sw = ApplicationSwitcher::new();
        sw.register_visual("app1", VisualId(1));
        sw.register_visual("app2", VisualId(2));
        assert_eq!(sw.all_app_ids().len(), 2);
    }
}
