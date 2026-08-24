use smithay::input::pointer::PointerHandle;
use smithay::input::SeatHandler;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::pointer_constraints::{
    with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
    PointerConstraint,
};
use tracing::info;

use crate::compositor::LookingGlass;

pub struct PointerConstraints {
    pub state: PointerConstraintsState,
    pub pointer_locked: bool,
    pub locked_surface: Option<WlSurface>,
}

impl PointerConstraints {
    pub fn new(display: &DisplayHandle) -> Self {
        let state = PointerConstraintsState::new::<LookingGlass>(display);
        PointerConstraints {
            state,
            pointer_locked: false,
            locked_surface: None,
        }
    }

    pub fn has_constraint_for(&self, surface: &WlSurface) -> bool {
        self.locked_surface.as_ref().map_or(false, |s| s == surface)
    }

    pub fn unlock(&mut self) {
        if self.pointer_locked {
            self.pointer_locked = false;
            self.locked_surface = None;
            info!("pointer unlocked");
        }
    }
}

impl PointerConstraintsHandler for LookingGlass {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        info!("new pointer constraint for surface");
        with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(c) = constraint {
                match &*c {
                    PointerConstraint::Locked(_) => {
                        self.pointer_constraints.pointer_locked = true;
                        self.pointer_constraints.locked_surface = Some(surface.clone());
                        c.activate();
                        info!("pointer locked");
                    }
                    PointerConstraint::Confined(_) => {
                        c.activate();
                        info!("pointer confined");
                    }
                }
            }
        });
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
    }
}
