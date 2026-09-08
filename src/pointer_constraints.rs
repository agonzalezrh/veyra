use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::pointer_constraints::{
    with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
    PointerConstraint,
};
use tracing::info;

use crate::compositor::LookingGlass;

#[allow(dead_code)] // reserved API surface; not yet wired
pub struct PointerConstraints {
    pub state: PointerConstraintsState,
    pub pointer_locked: bool,
    pub locked_surface: Option<WlSurface>,
    /// R7: surface with an active confinement (was previously untracked,
    /// so confinement had no routing effect at all).
    pub confined_surface: Option<WlSurface>,
}

impl PointerConstraints {
    pub fn new(display: &DisplayHandle) -> Self {
        let state = PointerConstraintsState::new::<LookingGlass>(display);
        PointerConstraints {
            state,
            pointer_locked: false,
            locked_surface: None,
            confined_surface: None,
        }
    }

    /// Local-state reset for the locked pointer. Protocol deactivation
    /// is the compositor's job — see LookingGlass::unlock_pointer.
    pub fn clear_locked(&mut self) {
        if self.pointer_locked {
            self.pointer_locked = false;
            self.locked_surface = None;
            info!("pointer lock state cleared");
        }
    }

    /// Local-state reset for confinement.
    pub fn clear_confined(&mut self) {
        if self.confined_surface.take().is_some() {
            info!("pointer confinement state cleared");
        }
    }
}

/// R7: clamp a compositor-space pointer position into the confinement
/// area: the intersection of the surface's content bounds and the
/// client-supplied region (translated from surface-local coordinates).
///
/// Region evaluation covers the common Add-rect case precisely (the
/// nearest containing rect wins); Subtract/Intersect kinds fall back
/// to the bounding box of the Add rects. A `None` region confines to
/// the whole surface content area, per the protocol.
pub fn constrain_to_region(
    proposed: (f64, f64),
    origin: (f64, f64),
    content_size: (f64, f64),
    region_rects: &[(smithay::wayland::compositor::RectangleKind, smithay::utils::Rectangle<i32, smithay::utils::Logical>)],
) -> (f64, f64) {
    // Content bounds in compositor space (origin is below the title bar).
    let mut min_x = origin.0;
    let mut min_y = origin.1;
    let mut max_x = origin.0 + content_size.0;
    let mut max_y = origin.1 + content_size.1;

    // Tighten with Add rects of the client region (surface-local →
    // compositor space).
    let add_rects: Vec<_> = region_rects
        .iter()
        .filter(|(kind, _)| {
            matches!(kind, smithay::wayland::compositor::RectangleKind::Add)
        })
        .map(|(_, r)| r)
        .collect();
    if !add_rects.is_empty() {
        let mut rmin_x = f64::INFINITY;
        let mut rmin_y = f64::INFINITY;
        let mut rmax_x = f64::NEG_INFINITY;
        let mut rmax_y = f64::NEG_INFINITY;
        for r in add_rects {
            rmin_x = rmin_x.min(origin.0 + r.loc.x as f64);
            rmin_y = rmin_y.min(origin.1 + r.loc.y as f64);
            rmax_x = rmax_x.max(origin.0 + (r.loc.x + r.size.w) as f64);
            rmax_y = rmax_y.max(origin.1 + (r.loc.y + r.size.h) as f64);
        }
        min_x = min_x.max(rmin_x);
        min_y = min_y.max(rmin_y);
        max_x = max_x.min(rmax_x);
        max_y = max_y.min(rmax_y);
    }

    // Degenerate (empty intersection): pin to the nearest clamp — the
    // pointer stays as close to the allowed area as possible instead of
    // teleporting.
    let cx = proposed.0.clamp(min_x, max_x.max(min_x));
    let cy = proposed.1.clamp(min_y, max_y.max(min_y));
    (cx, cy)
}

impl PointerConstraintsHandler for LookingGlass {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        info!("new pointer constraint for surface");
        with_pointer_constraint(surface, pointer, |constraint| {
            let Some(c) = constraint else { return };
            // R7: a constraint takes effect only while the pointer focus
            // is on the owning surface. An unfocused client requesting a
            // constraint stays PENDING — activation happens when focus
            // enters (activate_constraints_for_focus). Previously every
            // constraint activated immediately, letting an unfocused
            // client capture or confine the global pointer.
            let focused = pointer.current_focus().is_some_and(|f| f == *surface);
            match &*c {
                PointerConstraint::Locked(_) => {
                    if focused {
                        c.activate();
                        self.pointer_constraints.pointer_locked = true;
                        self.pointer_constraints.locked_surface = Some(surface.clone());
                        info!("pointer locked");
                    } else {
                        info!("pointer lock requested without pointer focus — pending");
                    }
                }
                PointerConstraint::Confined(_) => {
                    if focused {
                        c.activate();
                        self.pointer_constraints.confined_surface = Some(surface.clone());
                        info!("pointer confined");
                    } else {
                        info!("pointer confinement requested without pointer focus — pending");
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

#[cfg(test)]
mod tests {
    use super::constrain_to_region;
    use smithay::utils::{Point, Rectangle, Size};
    type R = (smithay::wayland::compositor::RectangleKind, Rectangle<i32, smithay::utils::Logical>);

    fn add(x: i32, y: i32, w: i32, h: i32) -> R {
        (
            smithay::wayland::compositor::RectangleKind::Add,
            Rectangle::new(Point::new(x, y), Size::new(w, h)),
        )
    }

    /// R7: without a client region, confinement is the whole surface
    /// content area (origin below the title bar).
    #[test]
    fn no_region_clamps_to_content_bounds() {
        let origin = (100.0, 50.0);
        let size = (200.0, 100.0);
        let (x, y) = constrain_to_region((500.0, 500.0), origin, size, &[]);
        assert_eq!((x, y), (300.0, 150.0), "clamped to origin+size");
        let (x, y) = constrain_to_region((50.0, 10.0), origin, size, &[]);
        assert_eq!((x, y), (100.0, 50.0), "clamped up to origin");
        let (x, y) = constrain_to_region((150.0, 80.0), origin, size, &[]);
        assert_eq!((x, y), (150.0, 80.0), "inside stays");
    }

    /// R7: a client region (surface-local) tightens the area further.
    #[test]
    fn client_region_tightens_confinement() {
        let origin = (100.0, 50.0);
        let size = (200.0, 100.0);
        let rects = vec![add(10, 10, 50, 25)];
        let (x, y) = constrain_to_region((250.0, 120.0), origin, size, &rects);
        // Region in compositor space: (110, 60) .. (160, 85)
        assert_eq!((x, y), (160.0, 85.0));
        let (x, y) = constrain_to_region((20.0, 20.0), origin, size, &rects);
        assert_eq!((x, y), (110.0, 60.0));
    }

    /// R7: a region outside the surface cannot widen confinement.
    #[test]
    fn region_cannot_exceed_surface() {
        let origin = (100.0, 50.0);
        let size = (200.0, 100.0);
        let rects = vec![add(-500, -500, 1000, 1000)];
        let (x, y) = constrain_to_region((400.0, 400.0), origin, size, &rects);
        assert_eq!((x, y), (300.0, 150.0), "content bounds still apply");
    }

    /// R7: empty intersection pins to the nearest allowed corner
    /// instead of producing an out-of-range or NaN position.
    #[test]
    fn degenerate_region_pins_to_corner() {
        let origin = (100.0, 50.0);
        let size = (200.0, 100.0);
        // Region entirely LEFT of the surface.
        let rects = vec![add(-300, -300, 200, 200)];
        let (x, y) = constrain_to_region((150.0, 80.0), origin, size, &rects);
        assert!(x.is_finite() && y.is_finite());
        assert_eq!((x, y), (100.0, 50.0), "pinned to the nearest allowed point");
    }
}
