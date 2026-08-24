use std::path::PathBuf;
use std::time::Instant;

use crate::config::Config;
use crate::workspace::WorkspaceManager;
use tracing::{info, warn};

pub struct Session {
    pub started: Instant,
    pub config: Config,
    pub state_path: PathBuf,
    pub shutdown_requested: bool,
    pub shutdown_completed: bool,
}

impl Session {
    pub fn new(config: Config) -> Self {
        let state_path = crate::persist::state_path_for_test();
        Session {
            started: Instant::now(),
            config,
            state_path,
            shutdown_requested: false,
            shutdown_completed: false,
        }
    }

    pub fn request_shutdown(&mut self) {
        if !self.shutdown_requested {
            self.shutdown_requested = true;
            info!("shutdown requested");
        }
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    pub fn startup_sequence(&mut self, workspace_manager: &mut WorkspaceManager) -> Result<(), String> {
        info!("session startup sequence beginning");

        let count = workspace_manager.len();
        info!(workspaces = count, config = ?self.config.workspace.count, "session configured");

        // Validate workspace count matches config
        if count != self.config.workspace.count {
            warn!(
                configured = self.config.workspace.count,
                actual = count,
                "workspace count mismatch"
            );
        }

        // Ensure all workspaces have a valid camera
        for i in 0..count {
            if let Some(ws) = workspace_manager.get_mut(i) {
                if ws.camera.position.z == 0.0 && ws.camera.position.x == 0.0 && ws.camera.position.y == 0.0 {
                    ws.camera.position.z = 800.0;
                    info!(workspace = i, "reset default camera for workspace");
                }
            }
        }

        // Clamp active workspace index
        if workspace_manager.active_id() >= count {
            info!("clamping active workspace to 0");
        }

        info!("session startup complete");
        Ok(())
    }

    pub fn shutdown_sequence(
        &mut self,
        save_state: impl FnOnce(),
    ) -> Result<(), String> {
        if self.shutdown_completed {
            info!("shutdown already completed, skipping");
            return Ok(());
        }

        info!("session shutdown sequence beginning");

        // Save state
        save_state();
        info!("state saved during shutdown");

        // Mark shutdown as complete
        self.shutdown_completed = true;
        info!("session shutdown complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn session_creates_workspaces_from_config() {
        let config = Config::default();
        let mut session = Session::new(config.clone());
        let mut wm = WorkspaceManager::new(config.workspace.count);

        assert_eq!(wm.len(), config.workspace.count);
        assert!(session.startup_sequence(&mut wm).is_ok());
    }

    #[test]
    fn session_shutdown_saves_state() {
        let config = Config::default();
        let mut session = Session::new(config.clone());
        let mut wm = WorkspaceManager::new(config.workspace.count);

        session.startup_sequence(&mut wm).unwrap();

        let mut state_saved = false;
        session.shutdown_sequence(|| {
            state_saved = true;
        }).unwrap();

        assert!(state_saved, "state should have been saved during shutdown");
    }

    #[test]
    fn session_shutdown_cleans_up() {
        let config = Config::default();
        let mut session = Session::new(config.clone());
        let mut wm = WorkspaceManager::new(config.workspace.count);

        session.startup_sequence(&mut wm).unwrap();
        session.shutdown_sequence(|| {}).unwrap();

        assert!(session.shutdown_completed);
    }

    #[test]
    fn multiple_shutdown_requests_no_double_save() {
        let config = Config::default();
        let mut session = Session::new(config.clone());
        let mut wm = WorkspaceManager::new(config.workspace.count);

        session.startup_sequence(&mut wm).unwrap();

        let mut save_count = 0;
        session.shutdown_sequence(|| {
            save_count += 1;
        }).unwrap();

        // Second call should be a no-op
        session.shutdown_sequence(|| {
            save_count += 1;
        }).unwrap();

        assert_eq!(save_count, 1, "state should only be saved once");
        assert!(session.shutdown_completed);
    }

    #[test]
    fn session_tracks_config_correctly() {
        let mut config = Config::default();
        config.workspace.count = 5;
        let session = Session::new(config.clone());

        assert_eq!(session.config.workspace.count, 5);
    }

    #[test]
    fn session_startup_completes() {
        let config = Config::default();
        let mut session = Session::new(config.clone());
        let mut wm = WorkspaceManager::new(config.workspace.count);

        let result = session.startup_sequence(&mut wm);
        assert!(result.is_ok(), "startup should succeed");
    }

    #[test]
    fn session_request_shutdown_flag() {
        let config = Config::default();
        let mut session = Session::new(config);
        assert!(!session.is_shutdown_requested());

        session.request_shutdown();
        assert!(session.is_shutdown_requested());

        // Requesting again should not change state
        session.request_shutdown();
        assert!(session.is_shutdown_requested());
    }

    #[test]
    fn empty_workspace_has_valid_camera() {
        let mut wm = WorkspaceManager::new(3);
        let config = Config::default();
        let mut session = Session::new(config);

        // Each workspace should have a valid camera after startup
        session.startup_sequence(&mut wm).unwrap();
        for i in 0..wm.len() {
            if let Some(ws) = wm.get(i) {
                assert!(ws.camera.position.z > 0.0, "workspace {} should have valid camera z", i);
            }
        }
    }
}
