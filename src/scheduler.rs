/// Tracks whether the compositor needs to render a new frame.
///
/// When idle (no input, no animation, no buffer changes), the compositor
/// should NOT render — achieving near-zero CPU usage.
///
/// The scheduler tracks two states:
/// - `dirty`: A new render is needed (buffer commit, transform change, input event, etc.)
/// - `animating`: A camera animation, focus transition, or overview transition is in progress
#[derive(Debug, Clone)]
pub struct RenderScheduler {
    dirty: bool,
    animating: bool,
}

impl RenderScheduler {
    pub fn new() -> Self {
        RenderScheduler {
            dirty: false,
            animating: false,
        }
    }

    /// Mark that a render is needed.
    pub fn schedule_render(&mut self) {
        self.dirty = true;
    }

    /// Mark that an animation is in progress.
    /// This keeps the render loop active even without explicit dirty triggers.
    pub fn set_animating(&mut self, animating: bool) {
        self.animating = animating;
        if animating {
            self.dirty = true;
        }
    }

    /// Whether a render is needed.
    pub fn needs_render(&self) -> bool {
        self.dirty || self.animating
    }

    /// Whether animation is currently active.
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    /// Called after rendering to clear the dirty flag.
    pub fn clear(&mut self) {
        self.dirty = false;
    }

    /// Reset both flags.
    pub fn reset(&mut self) {
        self.dirty = false;
        self.animating = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_starts_not_dirty() {
        let s = RenderScheduler::new();
        assert!(!s.needs_render());
    }

    #[test]
    fn schedule_render_makes_dirty() {
        let mut s = RenderScheduler::new();
        s.schedule_render();
        assert!(s.needs_render());
    }

    #[test]
    fn clear_after_render() {
        let mut s = RenderScheduler::new();
        s.schedule_render();
        s.clear();
        assert!(!s.needs_render());
    }

    #[test]
    fn animating_keeps_active() {
        let mut s = RenderScheduler::new();
        assert!(!s.needs_render());
        s.set_animating(true);
        assert!(s.needs_render());
        s.clear();
        // Still animating, so still needs render
        assert!(s.needs_render());
    }

    #[test]
    fn animation_ends() {
        let mut s = RenderScheduler::new();
        s.set_animating(true);
        s.set_animating(false);
        s.clear();
        assert!(!s.needs_render());
    }

    #[test]
    fn multiple_schedules_produce_one_render() {
        let mut s = RenderScheduler::new();
        s.schedule_render();
        s.schedule_render();
        s.schedule_render();
        assert!(s.needs_render());
        s.clear();
        assert!(!s.needs_render());
    }

    #[test]
    fn reset_clears_all() {
        let mut s = RenderScheduler::new();
        s.schedule_render();
        s.set_animating(true);
        s.reset();
        assert!(!s.needs_render());
        assert!(!s.is_animating());
    }
}
