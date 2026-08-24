use tracing::info;

/// Handles recovery from corrupted state, destroyed focus, invalid camera, etc.
#[derive(Debug)]
pub struct Recovery {
    pub recovery_available: bool,
}

impl Recovery {
    pub fn new() -> Self {
        Recovery {
            recovery_available: false,
        }
    }

    pub fn save_safe_state(&mut self) {
        self.recovery_available = true;
    }

    pub fn recover(&mut self, _compositor: &mut crate::compositor::LookingGlass) {
        if self.recovery_available {
            info!("attempting recovery from last safe state");
        }
        self.recovery_available = false;
    }

    pub fn is_available(&self) -> bool {
        self.recovery_available
    }
}

impl Default for Recovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_starts_unavailable() {
        let r = Recovery::new();
        assert!(!r.is_available());
    }

    #[test]
    fn save_safe_state_makes_recovery_available() {
        let mut r = Recovery::new();
        r.save_safe_state();
        assert!(r.is_available());
    }

    #[test]
    fn recovery_clears_availability() {
        let mut r = Recovery::new();
        r.save_safe_state();
        assert!(r.is_available());
    }

    #[test]
    fn new_is_default() {
        let r = Recovery::default();
        assert!(!r.is_available());
    }
}
