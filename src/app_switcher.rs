use crate::scene::VisualId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppId(pub String);

impl AppId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Application {
    pub id: AppId,
    pub name: String,
    pub visual_ids: Vec<VisualId>,
}

impl Application {
    pub fn new(id: AppId, name: String) -> Self {
        Application {
            id,
            name,
            visual_ids: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct ApplicationSwitcher {
    apps: Vec<AppId>,
    /// Most-recently-used order (most recent at front).
    mru: Vec<AppId>,
}

impl ApplicationSwitcher {
    pub fn new() -> Self {
        ApplicationSwitcher {
            apps: Vec::new(),
            mru: Vec::new(),
        }
    }

    pub fn register_visual(&mut self, app_id: &str, _visual_id: VisualId) {
        let id = AppId(app_id.to_string());
        if !self.apps.contains(&id) {
            self.apps.push(id.clone());
        }
        self.mru.retain(|a| a != &id);
        self.mru.insert(0, id);
    }

    pub fn unregister_visual(&mut self, app_id: &str, _visual_id: VisualId) {
        let id = AppId(app_id.to_string());
        self.mru.retain(|a| a != &id);
    }

    pub fn next(&mut self) -> Option<AppId> {
        if self.mru.is_empty() {
            return None;
        }
        // Rotate left: move first to end, return the new first (which was second)
        let current = self.mru.remove(0);
        self.mru.push(current);
        self.mru.first().cloned()
    }

    pub fn previous(&mut self) -> Option<AppId> {
        if self.mru.is_empty() {
            return None;
        }
        // Rotate right: move last to front, return the new first (which was last)
        let last = self.mru.pop()?;
        self.mru.insert(0, last);
        self.mru.first().cloned()
    }

    pub fn focus_app(&self, _app_id: &AppId) -> Option<VisualId> {
        None
    }

    pub fn mru_order(&self) -> &[AppId] {
        &self.mru
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
    fn two_apps_alternate() {
        let mut sw = ApplicationSwitcher::new();
        sw.register_visual("app1", VisualId(1));
        sw.register_visual("app2", VisualId(2));

        // Most recent is app2. next() goes to the previous app.
        let next = sw.next();
        assert_eq!(next.as_ref().map(|a| a.as_str()), Some("app1"));

        // Now next from app1 goes back to app2
        let next2 = sw.next();
        assert_eq!(next2.as_ref().map(|a| a.as_str()), Some("app2"));
    }

    #[test]
    fn cycle_through_all_apps() {
        let mut sw = ApplicationSwitcher::new();
        sw.register_visual("a", VisualId(1));
        sw.register_visual("b", VisualId(2));
        sw.register_visual("c", VisualId(3));

        // MRU: ["c", "b", "a"]
        // next() returns "b", MRU becomes ["a", "c", "b"]
        // next() returns "a", MRU becomes ["b", "a", "c"]
        // next() returns "b", MRU becomes ["c", "b", "a"]
        // next() returns "c", MRU becomes ["a", "c", "b"]
        // next() returns "a", MRU becomes ["b", "a", "c"]
        let mut seen = Vec::new();
        for _ in 0..6 {
            if let Some(id) = sw.next() {
                seen.push(id.as_str().to_string());
            }
        }
        assert_eq!(seen, vec!["b", "a", "c", "b", "a", "c"]);
    }

    #[test]
    fn previous_wraps() {
        let mut sw = ApplicationSwitcher::new();
        sw.register_visual("a", VisualId(1));
        sw.register_visual("b", VisualId(2));
        sw.register_visual("c", VisualId(3));

        // MRU: ["c", "b", "a"]
        // previous() rotates right: "a" moves to front
        // MRU: ["a", "c", "b"], returns "a"
        let prev = sw.previous();
        assert_eq!(prev.as_ref().map(|a| a.as_str()), Some("a"));
    }

    #[test]
    fn no_apps_returns_none() {
        let mut sw = ApplicationSwitcher::new();
        assert!(sw.next().is_none());
        assert!(sw.previous().is_none());
    }

    #[test]
    fn focus_app_returns_none() {
        let sw = ApplicationSwitcher::new();
        let app = AppId("test".to_string());
        assert!(sw.focus_app(&app).is_none());
    }
}
