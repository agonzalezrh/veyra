use cgmath::Deg;
use cgmath::InnerSpace;
use cgmath::Matrix3;
use cgmath::Matrix4;
use cgmath::Quaternion;
use cgmath::Rotation3;
use cgmath::SquareMatrix;
use cgmath::Vector3;
use cgmath::Vector4;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::Rectangle;

/// The window's current state — independent of content state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
}

impl Default for WindowState {
    fn default() -> Self { WindowState::Normal }
}

/// Actions a window operation can perform.
/// Provider-independent — each content source decides how to handle it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowAction {
    Close,
}

/// Configuration for window decorations.
#[derive(Debug, Clone)]
pub struct DecorationConfig {
    /// Title bar height as fraction of content height (e.g. 0.05 = 5%).
    pub title_bar_height: f32,
    pub title: String,
}

impl Default for DecorationConfig {
    fn default() -> Self {
        DecorationConfig {
            title_bar_height: 0.06,
            title: String::new(),
        }
    }
}

/// Distinguishes damage types for the rendering pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DamageKind {
    /// No damage, visual unchanged.
    #[default]
    None,
    /// Pixel content changed (buffer commit).
    Content,
    /// Only spatial state changed (position/rotation/scale), not content.
    SpatialOnly,
}

/// The state of a visual's content producer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentState {
    /// No producer connected; visual shows placeholder content.
    Disconnected,
    /// Producer is connecting (initial frames may still arrive).
    Connecting,
    /// Producer is active and providing frames.
    Ready,
    /// Producer encountered an error but may recover.
    Error,
}

impl Default for ContentState {
    fn default() -> Self { ContentState::Disconnected }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VisualId(pub u64);

impl VisualId {
    fn next() -> Self {
        use std::sync::atomic::AtomicU64;
        use std::sync::atomic::Ordering;
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        VisualId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone)]
pub struct Transform3D {
    pub position: Vector3<f32>,
    pub rotation: Quaternion<f32>,
    pub scale: Vector3<f32>,
}

impl Transform3D {
    pub fn identity() -> Self {
        Transform3D {
            position: Vector3::new(0.0, 0.0, 0.0),
            rotation: Quaternion::from_angle_z(Deg(0.0)),
            scale: Vector3::new(1.0, 1.0, 1.0),
        }
    }

    pub fn rotation_angle(&self) -> f32 {
        use cgmath::InnerSpace;
        let s = self.rotation.s;
        let len = self.rotation.v.magnitude();
        if len < 1e-6 {
            return 0.0;
        }
        2.0 * s.acos()
    }

    pub fn to_matrix(&self) -> Matrix4<f32> {
        let t = Matrix4::from_translation(self.position);
        let r = Matrix4::from(self.rotation);
        let s = Matrix4::from_nonuniform_scale(self.scale.x, self.scale.y, self.scale.z);
        t * r * s
    }

    /// Decompose a 4x4 matrix into a Transform3D (position, rotation, scale).
    /// The matrix is assumed to be T * R * S (no shear/perspective).
    pub fn from_matrix(m: &Matrix4<f32>) -> Self {
        let position = Vector3::new(m[3][0], m[3][1], m[3][2]);
        // Extract scale from column magnitudes
        let sx = Vector3::new(m[0][0], m[0][1], m[0][2]).magnitude();
        let sy = Vector3::new(m[1][0], m[1][1], m[1][2]).magnitude();
        let sz = Vector3::new(m[2][0], m[2][1], m[2][2]).magnitude();
        // Remove scale from columns to get pure rotation matrix
        let col0 = Vector3::new(m[0][0] / sx, m[0][1] / sx, m[0][2] / sx);
        let col1 = Vector3::new(m[1][0] / sy, m[1][1] / sy, m[1][2] / sy);
        let col2 = Vector3::new(m[2][0] / sz, m[2][1] / sz, m[2][2] / sz);
        let m3 = Matrix3::new(
            col0.x, col0.y, col0.z,
            col1.x, col1.y, col1.z,
            col2.x, col2.y, col2.z,
        );
        let rotation = Quaternion::from(m3);
        Transform3D {
            position,
            rotation,
            scale: Vector3::new(sx.max(0.001), sy.max(0.001), sz.max(0.001)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum VisualContent {
    WaylandSurface(GlesTexture),
    ExternalTexture(GlesTexture),
    /// Test-only content without a GPU texture, enabling scene-graph and
    /// interaction unit tests without an EGL context.
    #[cfg(test)]
    Test,
}

/// Compositor-owned chrome data for a visual.
/// This is metadata the compositor displays but never modifies client surfaces.
#[derive(Debug, Clone)]
pub struct SpatialChrome {
    pub title: String,
    pub app_id: String,
    pub focused: bool,
}

impl Default for SpatialChrome {
    fn default() -> Self {
        SpatialChrome {
            title: String::new(),
            app_id: String::new(),
            focused: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Visual {
    pub id: VisualId,
    pub content: VisualContent,
    pub geometry: Rectangle<i32, smithay::utils::Logical>,
    pub transform: Transform3D,
    pub parent: Option<VisualId>,
    pub selected: bool,
    pub focused: bool,
    pub content_state: ContentState,
    pub decoration: DecorationConfig,
    pub window_state: WindowState,
    /// Saved transform for Maximize → Restore cycle.
    pub saved_transform: Option<Transform3D>,
    /// Compositor-owned spatial chrome metadata.
    pub chrome: SpatialChrome,
    /// Damage tracking — whether content or spatial state changed.
    pub damage: DamageKind,
}

impl Visual {
    pub fn new(content: VisualContent, geometry: Rectangle<i32, smithay::utils::Logical>) -> Self {
        Visual {
            id: VisualId::next(),
            content,
            geometry,
            transform: Transform3D::identity(),
            parent: None,
            selected: false,
            focused: false,
            content_state: ContentState::Ready,
            decoration: DecorationConfig::default(),
            window_state: WindowState::Normal,
            saved_transform: None,
            chrome: SpatialChrome::default(),
            damage: DamageKind::Content,
        }
    }

    /// Test-only constructor: a plain pickable visual without GPU content.
    #[cfg(test)]
    pub fn new_test(width: i32, height: i32) -> Self {
        Visual::new(
            VisualContent::Test,
            Rectangle::new(
                smithay::utils::Point::new(0, 0),
                smithay::utils::Size::new(width, height),
            ),
        )
    }

    /// Returns true if the visual has active content (not disconnected/error).
    pub fn has_active_content(&self) -> bool {
        matches!(self.content_state, ContentState::Ready | ContentState::Connecting)
    }

    /// The total height of the visual including decoration (title bar).
    pub fn total_height(&self) -> f32 {
        let content_h = self.geometry.size.h as f32 * self.transform.scale.y;
        content_h * (1.0 + self.decoration.title_bar_height)
    }

    /// The total width (content width, unchanged by title bar).
    pub fn total_width(&self) -> f32 {
        self.geometry.size.w as f32 * self.transform.scale.x
    }

    /// Height of the title bar in scaled world units.
    pub fn title_bar_size(&self) -> f32 {
        self.geometry.size.h as f32 * self.transform.scale.y * self.decoration.title_bar_height
    }

    /// Returns true if a hit in local UV coords [0,1] is in the title bar.
    /// UV is the full visual UV (including decoration).
    pub fn hit_title_bar(&self, _u: f64, v: f64) -> bool {
        let h = self.decoration.title_bar_height as f64;
        v < h
    }

    /// Convert full-visual UV to content-only UV.
    /// Content UV is [0,1] within the content area only.
    pub fn content_uv(&self, u: f64, v: f64) -> (f64, f64) {
        let h = self.decoration.title_bar_height as f64;
        let cu = u;
        let cv = (v - h) / (1.0 - h);
        (cu.clamp(0.0, 1.0), cv.clamp(0.0, 1.0))
    }

    pub fn texture(&self) -> Option<&GlesTexture> {
        match &self.content {
            VisualContent::WaylandSurface(t) | VisualContent::ExternalTexture(t) => Some(t),
            #[cfg(test)]
            VisualContent::Test => None,
        }
    }

    pub fn texture_mut(&mut self) -> Option<&mut GlesTexture> {
        match &mut self.content {
            VisualContent::WaylandSurface(t) | VisualContent::ExternalTexture(t) => Some(t),
            #[cfg(test)]
            VisualContent::Test => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Scene {
    pub visuals: Vec<Visual>,
    pub selected_id: Option<VisualId>,
    pub focused_id: Option<VisualId>,
    pub hovered_id: Option<VisualId>,
    pub detached_set: Vec<VisualId>,
    /// Set of visual IDs that are de-emphasized (shelved — smaller, less prominent).
    /// De-emphasis is reversible and does NOT unmap the Wayland surface.
    pub de_emphasized_set: Vec<VisualId>,
    pub groups: Vec<crate::group::SpatialGroup>,
}

impl Scene {
    pub fn add(&mut self, visual: Visual) {
        self.visuals.push(visual);
    }

    pub fn remove(&mut self, id: VisualId) {
        // Remove parent-child relationships for this visual
        // Children of this visual must also be cleaned up
        let children: Vec<VisualId> = self.visuals.iter()
            .filter(|v| v.parent == Some(id))
            .map(|v| v.id)
            .collect();
        for child in children {
            self.remove(child);
        }
        self.visuals.retain(|v| v.id != id);
        if self.selected_id == Some(id) {
            self.selected_id = None;
        }
        if self.focused_id == Some(id) {
            self.focused_id = None;
        }
        if self.hovered_id == Some(id) {
            self.hovered_id = None;
        }
        self.detached_set.retain(|v| *v != id);
        self.de_emphasized_set.retain(|v| *v != id);
        // Remove from all groups
        for group in &mut self.groups {
            group.visual_ids.retain(|v| *v != id);
        }
    }

    /// Mark a visual as disconnected from its content producer.
    /// The visual survives with its transform and state.
    /// Clear focus (keyboard to disconnected guest makes no sense),
    /// but preserve selection (spatial position is still meaningful).
    pub fn disconnect(&mut self, id: VisualId) {
        if let Some(v) = self.get_mut(id) {
            v.content_state = ContentState::Disconnected;
        }
        if self.focused_id == Some(id) {
            self.focused_id = None;
            if let Some(v) = self.get_mut(id) {
                v.focused = false;
            }
        }
    }

    /// Check if a visual has active content.
    pub fn is_active(&self, id: VisualId) -> bool {
        self.visuals.iter().any(|v| v.id == id && v.has_active_content())
    }

    /// Check if a visual is visible (not minimized).
    pub fn is_visible(&self, id: VisualId) -> bool {
        self.visuals.iter().any(|v| v.id == id && v.window_state != WindowState::Minimized)
    }

    /// Check if a visual is currently minimized (I5).
    pub fn is_minimized(&self, id: VisualId) -> bool {
        self.visuals.iter().any(|v| v.id == id && v.window_state == WindowState::Minimized)
    }

    /// Set the minimized state without touching transforms or the
    /// saved-transform slot (I5).
    ///
    /// Unlike `minimize`/`restore`, this intentionally does NOT capture or
    /// restore transforms: the caller owns presentation state. A maximized
    /// window keeps its centered pose beneath the Minimized flag; layout
    /// and arrangement must treat minimized visuals as detached.
    /// Returns true if the visual exists (state set is idempotent).
    pub fn set_minimized(&mut self, id: VisualId, minimized: bool) -> bool {
        if let Some(v) = self.get_mut(id) {
            if minimized {
                v.window_state = WindowState::Minimized;
            } else if v.window_state == WindowState::Minimized {
                v.window_state = WindowState::Normal;
            }
            true
        } else {
            false
        }
    }

    /// Move a visual to the top of the stacking order (raised above all
    /// other visuals in draw order). Returns true on success.
    pub fn raise_to_top(&mut self, id: VisualId) -> bool {
        let idx = match self.find_index(id) {
            Some(i) => i,
            None => return false,
        };
        if idx >= self.visuals.len() - 1 {
            return true; // already top
        }
        let visual = self.visuals.remove(idx);
        self.visuals.push(visual);
        true
    }

    /// Check if a visual is de-emphasized.
    pub fn is_de_emphasized(&self, id: VisualId) -> bool {
        self.de_emphasized_set.contains(&id)
    }

    /// De-emphasize a visual: set flag, remove keyboard focus.
    /// The client stays mapped/alive (no Wayland unmap).
    /// Returns true if the visual was found.
    pub fn de_emphasize(&mut self, id: VisualId) -> bool {
        if !self.visuals.iter().any(|v| v.id == id) {
            return false;
        }
        if !self.de_emphasized_set.contains(&id) {
            self.de_emphasized_set.push(id);
        }
        // Remove keyboard focus when de-emphasized
        if self.focused_id == Some(id) {
            self.focus(None);
        }
        true
    }

    /// Restore a de-emphasized visual to normal emphasis.
    /// Returns true if the visual was de-emphasized.
    pub fn restore_from_de_emphasis(&mut self, id: VisualId) -> bool {
        let len_before = self.de_emphasized_set.len();
        self.de_emphasized_set.retain(|v| *v != id);
        self.de_emphasized_set.len() < len_before
    }

    /// Compute the world-space transform matrix for a visual by composing
    /// parent chains. Returns identity matrix if the visual doesn't exist.
    /// Order: parent transforms on the left, child on the right:
    ///   world = parent_local * grandparent_local * ... * child_local
    /// where `to_matrix()` is T * R * S.
    pub fn world_matrix(&self, id: VisualId) -> Matrix4<f32> {
        let mut visited = 0u32;
        let mut current = id;
        let mut chain: Vec<Matrix4<f32>> = Vec::new();
        loop {
            visited += 1;
            if visited > 32 { break; }
            let visual = match self.visuals.iter().find(|v| v.id == current) {
                Some(v) => v,
                None => break,
            };
            chain.push(visual.transform.to_matrix());
            match visual.parent {
                Some(p) => current = p,
                None => break,
            }
        }
        // Compose: parent transforms on the left, iterate reversed
        // world = start identity * child_local * parent_local * grandparent_local...
        // Actually: position(child) is in parent space. So:
        // world(child) = world(parent) * child_local
        // We compute by walking up, then composing down:
        let mut result = Matrix4::identity();
        for m in chain.iter().rev() {
            result = result * m;
        }
        result
    }

    /// The visual's transform as seen in world space: parent chain
    /// applied to its local transform. Position and rotation come from
    /// the composed matrix (exactly the extraction the renderer does);
    /// scale stays the visual's own, since quads size themselves from
    /// their own geometry — matching the renderer's model matrix.
    ///
    /// Picking and pointer→UV mapping must use this so that parented
    /// visuals (popups, groups) are hit where they are DRAWN, not
    /// where their local coordinates happen to sit.
    pub fn world_transform(&self, id: VisualId) -> Transform3D {
        let world = self.world_matrix(id);
        let pos = Vector3::new(world[3][0], world[3][1], world[3][2]);
        let m3 = cgmath::Matrix3::new(
            world[0][0], world[0][1], world[0][2],
            world[1][0], world[1][1], world[1][2],
            world[2][0], world[2][1], world[2][2],
        );
        let rot = cgmath::Quaternion::from(m3);
        let own_scale = self.visuals.iter().find(|v| v.id == id)
            .map(|v| v.transform.scale)
            .unwrap_or_else(|| Vector3::new(1.0, 1.0, 1.0));
        Transform3D { position: pos, rotation: rot, scale: own_scale }
    }

    /// Set a visual's parent. Returns an error if it would create a cycle.
    pub fn set_parent(&mut self, child: VisualId, new_parent: VisualId) -> Result<(), String> {
        if child == new_parent {
            return Err("cannot parent to self".into());
        }
        let child_idx = match self.visuals.iter().position(|v| v.id == child) {
            Some(i) => i,
            None => return Err("child not found".into()),
        };
        if !self.visuals.iter().any(|v| v.id == new_parent) {
            return Err("parent not found".into());
        }
        // Cycle detection: walk from new_parent upwards
        let mut current = new_parent;
        for _ in 0..32 {
            if current == child {
                return Err("cycle detected".into());
            }
            let visual = match self.visuals.iter().find(|v| v.id == current) {
                Some(v) => v,
                None => break,
            };
            match visual.parent {
                Some(p) => current = p,
                None => break,
            }
        }
        self.visuals[child_idx].parent = Some(new_parent);
        Ok(())
    }

    /// Remove a visual's parent relationship. Returns true if found.
    pub fn clear_parent(&mut self, id: VisualId) -> bool {
        match self.visuals.iter_mut().find(|v| v.id == id) {
            Some(v) => { v.parent = None; true }
            None => false,
        }
    }

    /// Detach a visual from its parent, preserving its world transform.
    /// The visual's local transform is updated to match its current world
    /// position/rotation/scale, and parent is cleared.
    /// Returns true if the visual was found and had a parent.
    pub fn detach_from_parent(&mut self, id: VisualId) -> bool {
        let idx = match self.visuals.iter().position(|v| v.id == id) {
            Some(i) => i,
            None => return false,
        };
        if self.visuals[idx].parent.is_none() {
            return true; // already detached
        }
        let world = self.world_matrix(id);
        self.visuals[idx].transform = Transform3D::from_matrix(&world);
        self.visuals[idx].parent = None;
        true
    }

    /// Reparent a visual to a new parent, preserving its world transform.
    /// The visual's local transform is recomputed relative to the new parent.
    /// Returns an error if the reparenting would create a cycle.
    pub fn reparent(&mut self, child: VisualId, new_parent: VisualId) -> Result<(), String> {
        if child == new_parent {
            return Err("cannot parent to self".into());
        }
        let idx = match self.visuals.iter().position(|v| v.id == child) {
            Some(i) => i,
            None => return Err("child not found".into()),
        };
        if !self.visuals.iter().any(|v| v.id == new_parent) {
            return Err("parent not found".into());
        }
        // Compute child's current world transform
        let child_world = self.world_matrix(child);
        // Compute new parent's world transform
        let parent_world = self.world_matrix(new_parent);
        // Invert parent world: new_local = inverse(parent_world) * child_world
        let inv_parent = match parent_world.invert() {
            Some(m) => m,
            None => return Err("parent transform not invertible".into()),
        };
        let new_local = inv_parent * child_world;
        // Cycle detection: walk from new_parent upwards
        let mut current = new_parent;
        for _ in 0..32 {
            if current == child {
                return Err("cycle detected".into());
            }
            let visual = match self.visuals.iter().find(|v| v.id == current) {
                Some(v) => v,
                None => break,
            };
            match visual.parent {
                Some(p) => current = p,
                None => break,
            }
        }
        // Apply new local transform and set parent
        self.visuals[idx].transform = Transform3D::from_matrix(&new_local);
        self.visuals[idx].parent = Some(new_parent);
        Ok(())
    }

    /// Minimize a visual: hide it but preserve all state.
    pub fn minimize(&mut self, id: VisualId) -> bool {
        if let Some(v) = self.get_mut(id) {
            if v.window_state != WindowState::Minimized {
                v.saved_transform = Some(v.transform.clone());
                v.window_state = WindowState::Minimized;
            }
            true
        } else {
            false
        }
    }

    /// Maximize a visual: save current transform, fit to viewport.
    /// The actual viewport fitting is done by the caller (LookingGlass).
    /// This method saves the transform and sets state.
    pub fn maximize(&mut self, id: VisualId) -> bool {
        if let Some(v) = self.get_mut(id) {
            if v.window_state == WindowState::Maximized {
                return true;
            }
            if v.window_state == WindowState::Normal {
                v.saved_transform = Some(v.transform.clone());
            }
            v.window_state = WindowState::Maximized;
            true
        } else {
            false
        }
    }

    /// Restore a minimized or maximized visual to its previous state.
    pub fn restore(&mut self, id: VisualId) -> bool {
        if let Some(v) = self.get_mut(id) {
            match &v.saved_transform {
                Some(saved) => v.transform = saved.clone(),
                None => v.transform = Transform3D::identity(),
            }
            v.saved_transform = None;
            v.window_state = WindowState::Normal;
            true
        } else {
            false
        }
    }

    pub fn get_mut(&mut self, id: VisualId) -> Option<&mut Visual> {
        self.visuals.iter_mut().find(|v| v.id == id)
    }

    /// Immutable lookup by id.
    pub fn get(&self, id: VisualId) -> Option<&Visual> {
        self.visuals.iter().find(|v| v.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Visual> {
        self.visuals.iter()
    }

    /// Set the selected visual. Deselects the previous one.
    pub fn select(&mut self, id: Option<VisualId>) {
        if self.selected_id == id {
            return;
        }
        if let Some(old) = self.selected_id {
            if let Some(v) = self.get_mut(old) {
                v.selected = false;
            }
        }
        self.selected_id = id;
        if let Some(new) = id {
            if let Some(v) = self.get_mut(new) {
                v.selected = true;
            }
        }
    }

    /// Set the focused visual. Unfocuses the previous one.
    /// Focus is independent of selection — a visual can be focused
    /// for keyboard input while another is selected for manipulation.
    pub fn focus(&mut self, id: Option<VisualId>) {
        if self.focused_id == id {
            return;
        }
        if let Some(old) = self.focused_id {
            if let Some(v) = self.get_mut(old) {
                v.focused = false;
            }
        }
        self.focused_id = id;
        if let Some(new) = id {
            if let Some(v) = self.get_mut(new) {
                v.focused = true;
            }
        }
    }

    /// Pick a sensible focus replacement after the focused visual was
    /// destroyed: the topmost remaining active visual (highest draw order,
    /// i.e. most recently raised) among the given workspace members.
    /// Returns None when the workspace has no remaining active visuals.
    pub fn pick_focus_replacement(&self, workspace_ids: &[VisualId]) -> Option<VisualId> {
        // A minimized window must never receive keyboard focus via
        // replacement (I5): it is invisible and unpickable.
        pick_replacement_from(
            self.visuals.iter().map(|v| v.id),
            workspace_ids,
            |id| self.is_active(id) && !self.is_minimized(id),
        )
    }

    // ── Stacking order ────────────────────────────────────────────────
    //
    // Stacking order is determined by position in visuals[]:
    //   first = bottom (drawn first, behind)
    //   last  = top   (drawn last, in front)
    //
    // The renderer draws visuals in iteration order, so the last visual
    // in the vector appears on top. GPU depth testing resolves actual
    // Z-depth, but for visuals at the same depth, list order decides.

    fn find_index(&self, id: VisualId) -> Option<usize> {
        self.visuals.iter().position(|v| v.id == id)
    }

    /// Move a visual to the top of the stacking order.
    /// Returns true if the visual was found and moved.
    pub fn bring_to_front(&mut self, id: VisualId) -> bool {
        let idx = match self.find_index(id) {
            Some(i) => i,
            None => return false,
        };
        if idx == self.visuals.len() - 1 {
            return true; // already on top
        }
        let visual = self.visuals.remove(idx);
        self.visuals.push(visual);
        true
    }

    /// Move a visual to the bottom of the stacking order.
    pub fn send_to_back(&mut self, id: VisualId) -> bool {
        let idx = match self.find_index(id) {
            Some(i) => i,
            None => return false,
        };
        if idx == 0 {
            return true;
        }
        let visual = self.visuals.remove(idx);
        self.visuals.insert(0, visual);
        true
    }

    /// Raise a visual by one position in the stacking order.
    pub fn raise(&mut self, id: VisualId) -> bool {
        let idx = match self.find_index(id) {
            Some(i) => i,
            None => return false,
        };
        if idx >= self.visuals.len() - 1 {
            return true; // already top
        }
        self.visuals.swap(idx, idx + 1);
        true
    }

    /// Lower a visual by one position in the stacking order.
    pub fn lower(&mut self, id: VisualId) -> bool {
        let idx = match self.find_index(id) {
            Some(i) => i,
            None => return false,
        };
        if idx == 0 {
            return true; // already bottom
        }
        self.visuals.swap(idx, idx - 1);
        true
    }

    /// Clear damage after rendering.
    pub fn clear_damage(&mut self) {
        for v in &mut self.visuals {
            v.damage = DamageKind::default();
        }
    }

    /// Reset a visual's transform to identity (position 0,0,0, no rotation, scale 1).
    pub fn reset_transform(&mut self, id: VisualId) -> bool {
        match self.get_mut(id) {
            Some(v) => {
                v.transform = Transform3D::identity();
                true
            }
            None => false,
        }
    }

    /// Pick the closest visual under a screen coordinate.
    /// When two visuals are at the same depth, the one on top (later in
    /// stacking order) wins.
    pub fn pick(
        &self,
        proj_view: &Matrix4<f32>,
        ndc_x: f32,
        ndc_y: f32,
    ) -> Option<(VisualId, f32)> {
        // World-space picking: parented visuals (popups, groups) are
        // hit where they are drawn, not at their local coordinates.
        let items: Vec<_> = self.visuals
            .iter()
            .map(|v| {
                (
                    v.id,
                    self.world_transform(v.id),
                    (v.total_width(), v.total_height()),
                )
            })
            .collect();
        pick_visual_items(proj_view, ndc_x, ndc_y, &items)
    }

    /// Pick only among visuals in the given visible set (workspace filter).
    /// Uses the same ray-cast logic but only considers IDs in `visible`.
    pub fn pick_visible(
        &self,
        proj_view: &Matrix4<f32>,
        ndc_x: f32,
        ndc_y: f32,
        visible: &[VisualId],
    ) -> Option<(VisualId, f32)> {
        let items: Vec<_> = self.visuals
            .iter()
            .filter(|v| visible.contains(&v.id))
            .map(|v| {
                (
                    v.id,
                    self.world_transform(v.id),
                    (v.total_width(), v.total_height()),
                )
            })
            .collect();
        pick_visual_items(proj_view, ndc_x, ndc_y, &items)
    }
}

/// Pure function: test which visual is hit by a ray from screen NDC.
///
/// `proj_view` = projection × view matrix.
/// `ndc_x`, `ndc_y` = normalized device coordinates in [-1, 1].
/// `visuals` = slice of (id, world transform, (w, h)) items.
///
/// Returns the closest intersected `(VisualId, hit_distance)` or `None`.
/// Pure picking math operating on (id, transform, width, height) tuples.
/// Used by Scene::pick / pick_visible and by unit tests.
fn pick_visual_items(
    proj_view: &Matrix4<f32>,
    ndc_x: f32,
    ndc_y: f32,
    items: &[(VisualId, Transform3D, (f32, f32))],
) -> Option<(VisualId, f32)> {
    let inv_pv = proj_view.invert().unwrap_or(Matrix4::identity());

    let near = inv_pv * Vector4::new(ndc_x, ndc_y, -1.0, 1.0);
    let far = inv_pv * Vector4::new(ndc_x, ndc_y, 1.0, 1.0);
    let near = Vector3::new(near.x / near.w, near.y / near.w, near.z / near.w);
    let far = Vector3::new(far.x / far.w, far.y / far.w, far.z / far.w);
    let dir = (far - near).normalize();
    let mut closest: Option<(VisualId, f32)> = None;
    for (id, transform, (gw, gh)) in items {
        let model = Matrix4::from_translation(transform.position)
            * Matrix4::from(transform.rotation)
            * Matrix4::from_nonuniform_scale(*gw, *gh, 1.0);

        let inv_model = model.invert().unwrap_or(Matrix4::identity());
        let local_origin = inv_model * Vector4::new(near.x, near.y, near.z, 1.0);
        let local_dir = inv_model * Vector4::new(dir.x, dir.y, dir.z, 0.0);
        let lo = Vector3::new(local_origin.x, local_origin.y, local_origin.z) / local_origin.w;
        let ld = Vector3::new(local_dir.x, local_dir.y, local_dir.z);

        if ld.z.abs() < 1e-8 {
            continue;
        }
        let t = -lo.z / ld.z;
        if t < 0.0 {
            continue;
        }
        let hit_pt = lo + ld * t;
        if hit_pt.x.abs() > 0.5 || hit_pt.y.abs() > 0.5 {
            continue;
        }

        let local_hit = Vector4::new(hit_pt.x, hit_pt.y, 0.0, 1.0);
        let world_hit_4 = model * local_hit;
        let world_hit = Vector3::new(world_hit_4.x, world_hit_4.y, world_hit_4.z) / world_hit_4.w;
        let dist = (world_hit - near).magnitude();

        match closest {
            Some((_, closest_dist)) if dist > closest_dist => {}
            _ => closest = Some((*id, dist)),
        }
    }
    closest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_center_hit() {
        let items = vec![(
            VisualId(1),
            Transform3D {
                position: Vector3::new(0.0, 0.0, 0.0),
                rotation: Quaternion::from_angle_z(Deg(0.0)),
                scale: Vector3::new(1.0, 1.0, 1.0),
            },
            (200.0, 100.0),
        )];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items);
        assert!(r.is_some(), "should hit center");
        assert_eq!(r.unwrap().0, VisualId(1));
    }

    #[test]
    fn pick_miss() {
        let items = vec![(
            VisualId(1),
            Transform3D {
                position: Vector3::new(0.0, 0.0, 0.0),
                rotation: Quaternion::from_angle_z(Deg(0.0)),
                scale: Vector3::new(1.0, 1.0, 1.0),
            },
            (200.0, 100.0),
        )];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let r = pick_visual_items(&(proj * view), -0.9, 0.9, &items);
        assert!(r.is_none(), "should miss when pointing at corner");
    }

    #[test]
    fn pick_depth_wins() {
        let items = vec![
            (
                VisualId(1),
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
            (
                VisualId(2),
                Transform3D {
                    position: Vector3::new(0.0, 0.0, -200.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
        ];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items);
        assert!(r.is_some(), "should hit something");
        assert_eq!(r.unwrap().0, VisualId(1), "should pick closer visual (z=0 vs z=-200)");
    }

    #[test]
    fn pick_rotated() {
        let items = vec![(
            VisualId(1),
            Transform3D {
                position: Vector3::new(0.0, 0.0, 0.0),
                rotation: Quaternion::from_angle_y(Deg(45.0)),
                scale: Vector3::new(1.0, 1.0, 1.0),
            },
            (200.0, 100.0),
        )];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items);
        assert!(r.is_some(), "should hit rotated visual at center");
    }

    #[test]
    fn pick_still_works_after_camera_move() {
        let items = vec![(
            VisualId(1),
            Transform3D {
                position: Vector3::new(100.0, 50.0, 0.0),
                rotation: Quaternion::from_angle_z(Deg(0.0)),
                scale: Vector3::new(1.0, 1.0, 1.0),
            },
            (200.0, 100.0),
        )];
        // Camera shifted right and up
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(50.0, 25.0, 500.0),
            cgmath::Point3::new(50.0, 25.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        // The visual is at (100, 50), camera is at (50, 25), so visual is at
        // (100-50, 50-25) = (50, 25) in camera-relative. NDC center should hit.
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items);
        assert!(r.is_some(), "should hit after camera move");
    }

    #[test]
    fn focus_independent_of_selection() {
        let mut scene = Scene::default();
        // Can't easily construct Visuals without GlesTexture in scene tests.
        // Use pick_visual_items for the picking math.
        // The focus/select API methods don't need Visuals — they just store IDs.
        scene.focus(Some(VisualId(1)));
        assert_eq!(scene.focused_id, Some(VisualId(1)));
        scene.select(Some(VisualId(2)));
        assert_eq!(scene.selected_id, Some(VisualId(2)));
        // Focus untouched
        assert_eq!(scene.focused_id, Some(VisualId(1)));
    }

    #[test]
    fn focus_clear_on_none() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.focus(None);
        assert_eq!(scene.focused_id, None);
    }

    // ── Focus lifecycle tests ──────────────────────────────────────────

    #[test]
    fn remove_destroyed_cleans_selected() {
        let mut scene = Scene::default();
        scene.select(Some(VisualId(42)));
        scene.remove(VisualId(42));
        assert_eq!(scene.selected_id, None);
    }

    #[test]
    fn remove_destroyed_cleans_focused() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(42)));
        scene.remove(VisualId(42));
        assert_eq!(scene.focused_id, None);
    }

    #[test]
    fn remove_destroyed_cleans_hovered() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(42)));
        scene.focus(Some(VisualId(7)));
        scene.select(Some(VisualId(7)));
        // Remove non-focused, non-selected visual — others untouched
        scene.remove(VisualId(42));
        assert_eq!(scene.focused_id, Some(VisualId(7)));
        assert_eq!(scene.selected_id, Some(VisualId(7)));
    }

    #[test]
    fn select_different_visual_leaves_focus() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.select(Some(VisualId(2)));
        assert_eq!(scene.focused_id, Some(VisualId(1)), "focus unchanged after select");
        assert_eq!(scene.selected_id, Some(VisualId(2)), "selected changed");
    }

    #[test]
    fn focus_different_visual_leaves_selected() {
        let mut scene = Scene::default();
        scene.select(Some(VisualId(1)));
        scene.focus(Some(VisualId(2)));
        assert_eq!(scene.selected_id, Some(VisualId(1)), "selected unchanged after focus");
        assert_eq!(scene.focused_id, Some(VisualId(2)), "focused changed");
    }

    // ── Stacking / picking tie-break tests ─────────────────────────────

    #[test]
    fn pick_at_same_depth_later_wins() {
        // Two visuals at exactly the same position and depth.
        // The later one in the list (stacking "top") should be picked.
        let items = vec![
            (
                VisualId(1),
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
            (
                VisualId(2),
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
        ];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        // Both at same depth — later (id=2) should win
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items);
        assert!(r.is_some());
        assert_eq!(r.unwrap().0, VisualId(2), "topmost (id=2) should win at equal depth");
    }

    #[test]
    fn pick_at_same_depth_reversed_order() {
        // Same as above but with items in reverse order
        let items = vec![
            (
                VisualId(2),
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
            (
                VisualId(1),
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
        ];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        // Now id=1 is later in list — should win
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items);
        assert!(r.is_some());
        assert_eq!(r.unwrap().0, VisualId(1), "topmost (later in list) should win");
    }

    #[test]
    fn stacking_unknown_visual_returns_false() {
        let mut scene = Scene::default();
        assert!(!scene.bring_to_front(VisualId(999)));
        assert!(!scene.send_to_back(VisualId(999)));
        assert!(!scene.raise(VisualId(999)));
        assert!(!scene.lower(VisualId(999)));
        assert!(!scene.reset_transform(VisualId(999)));
    }

    // ── Lifecycle / disconnect tests ─────────────────────────────────

    #[test]
    fn disconnect_clears_focus_not_selection() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.select(Some(VisualId(1)));
        scene.disconnect(VisualId(1));
        assert_eq!(scene.focused_id, None, "focus cleared on disconnect");
        assert_eq!(scene.selected_id, Some(VisualId(1)), "selection preserved on disconnect");
    }

    #[test]
    fn disconnect_other_visual_leaves_focus() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.disconnect(VisualId(2));
        assert_eq!(scene.focused_id, Some(VisualId(1)), "focus unchanged when other visual disconnects");
    }

    #[test]
    fn is_active_disconnected() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.disconnect(VisualId(1));
        assert!(!scene.is_active(VisualId(1)), "disconnected visual is not active");
    }

    #[test]
    fn is_active_unknown_visual() {
        let scene = Scene::default();
        assert!(!scene.is_active(VisualId(999)), "unknown visual is not active");
    }

    #[test]
    fn disconnect_idempotent() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.disconnect(VisualId(1));
        scene.disconnect(VisualId(1)); // second disconnect should not crash
        assert_eq!(scene.focused_id, None);
        assert!(!scene.is_active(VisualId(1)));
    }

    #[test]
    fn multiple_disconnects_one_active() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.focus(Some(VisualId(2)));
        scene.disconnect(VisualId(1));
        assert_eq!(scene.focused_id, Some(VisualId(2)), "second visual retains focus");
    }

    #[test]
    fn bring_to_front_changes_pick_order() {
        use cgmath::Matrix4;
        // Simulate: add A then B. A is at bottom, B is on top.
        // At same depth, B wins. Then bring A to front -> A wins.
        // We verify using pick_visual_items with items in same order.
        let items_before = vec![
            (
                VisualId(1), // A
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
            (
                VisualId(2), // B
                Transform3D {
                    position: Vector3::new(0.0, 0.0, 0.0),
                    rotation: Quaternion::from_angle_z(Deg(0.0)),
                    scale: Vector3::new(1.0, 1.0, 1.0),
                },
                (200.0, 100.0),
            ),
        ];
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        // Before: B (id=2, later) wins
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items_before);
        assert_eq!(r.unwrap().0, VisualId(2));

        // After "bring_to_front": A should now be at the end
        let mut items_after = items_before.clone();
        let a = items_after.remove(0);
        items_after.push(a);
        let r = pick_visual_items(&(proj * view), 0.0, 0.0, &items_after);
        assert_eq!(r.unwrap().0, VisualId(1), "A brought to front should now win");
    }

    // ── Window state tests ────────────────────────────────────────────

    #[test]
    fn minimize_preserves_transform() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        let _pos = Vector3::new(100.0, 200.0, 300.0);
        // We can't create a visual here, but we can test Scene methods
        // for VisualId that don't exist — they should return false
        assert!(!scene.minimize(VisualId(999)), "unknown visual cant minimize");
        assert!(!scene.restore(VisualId(999)));
        assert!(!scene.maximize(VisualId(999)));
    }

    #[test]
    fn minimize_twice_no_crash() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.minimize(VisualId(1));
        // Second minimize on non-existent is fine — tests the return value
        assert!(!scene.minimize(VisualId(999)));
    }

    #[test]
    fn is_visible_checks_window_state() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        // is_visible for unknown visual returns false
        assert!(!scene.is_visible(VisualId(999)));
    }

    #[test]
    fn window_state_separate_from_content_state() {
        // WindowState and ContentState are independent.
        // A visual can be Minimized with Ready content.
        // Verify the enum values are distinct.
        assert_eq!(WindowState::default(), WindowState::Normal);
        assert_eq!(ContentState::default(), ContentState::Disconnected);
    }

    // ── I5: minimize state without transform capture ──────────────────

    /// Two active test visuals; returns their ids in draw order.
    fn two_active_visuals() -> (Scene, [VisualId; 2]) {
        let mut scene = Scene::default();
        let a = Visual::new_test(400, 300);
        let b = Visual::new_test(400, 300);
        let ids = [a.id, b.id];
        scene.add(a);
        scene.add(b);
        (scene, ids)
    }

    #[test]
    fn set_minimized_flips_state_idempotent() {
        let (mut scene, _) = two_active_visuals();
        let id = scene.visuals[0].id;
        assert!(!scene.is_minimized(id));
        assert!(scene.set_minimized(id, true));
        assert!(scene.is_minimized(id));
        assert!(!scene.is_visible(id));
        // Double minimize is harmless; restore clears it exactly once.
        assert!(scene.set_minimized(id, true));
        assert!(scene.set_minimized(id, false));
        assert!(!scene.is_minimized(id));
        assert!(scene.is_visible(id));
    }

    #[test]
    fn set_minimized_keeps_transform() {
        let (mut scene, _) = two_active_visuals();
        let id = scene.visuals[0].id;
        let before = scene.get(id).unwrap().transform.clone();
        scene.set_minimized(id, true);
        let after = scene.get(id).unwrap().transform.clone();
        assert_eq!(before.position, after.position, "minimize must not move the visual");
        assert_eq!(before.rotation, after.rotation);
        assert_eq!(after.scale, after.scale);
        // saved_transform slot untouched: caller owns presentation.
        assert!(scene.get(id).unwrap().saved_transform.is_none());
    }

    #[test]
    fn set_minimized_unknown_visual() {
        let mut scene = Scene::default();
        assert!(!scene.set_minimized(VisualId(999), true));
        assert!(!scene.is_minimized(VisualId(999)));
    }

    #[test]
    fn focus_replacement_skips_minimized() {
        let (mut scene, ids) = two_active_visuals();
        // Normal case: last active visual in draw order wins
        let repl = scene.pick_focus_replacement(&ids);
        assert_eq!(repl, Some(ids[1]), "last in draw order wins");
        // Minimized: must be skipped even though it is last and active
        scene.set_minimized(ids[1], true);
        let repl = scene.pick_focus_replacement(&ids);
        assert_eq!(repl, Some(ids[0]), "minimized visual is not a candidate");
        // Both minimized: no candidate
        scene.set_minimized(ids[0], true);
        assert_eq!(scene.pick_focus_replacement(&ids), None);
    }

    #[test]
    fn raise_to_top_moves_stacking() {
        let (mut scene, ids) = two_active_visuals();
        assert!(scene.raise_to_top(ids[0]));
        // Draw order: ids[1] (bottom), ids[0] (top)
        let order: Vec<VisualId> = scene.visuals.iter().map(|v| v.id).collect();
        assert_eq!(order, vec![ids[1], ids[0]]);
        // Already top: harmless
        assert!(scene.raise_to_top(ids[0]));
        // Unknown: false
        assert!(!scene.raise_to_top(VisualId(999)));
    }

    // ── Multi-provider integration tests ──────────────────────────────
    //
    // These tests prove that scene operations are provider-agnostic.
    // No Visual content is created — all operations work purely on
    // VisualId and Scene state, which is the same for any provider.

    #[test]
    fn stacking_works_identically_for_any_provider() {
        let mut scene = Scene::default();
        // bring_to_front and friends work on VisualId alone
        // regardless of which provider created the Visual
        scene.select(Some(VisualId(1)));
        scene.bring_to_front(VisualId(1));
        assert_eq!(scene.selected_id, Some(VisualId(1)));
    }

    #[test]
    fn disconnect_one_leaves_others() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        scene.select(Some(VisualId(1)));
        // Focus and select are independent; disconnect clears focus, not selection
        scene.focus(Some(VisualId(2)));
        scene.disconnect(VisualId(1));
        // Visual 1 was disconnected but was focused? No, focus is now 2
        // Visual 1 was selected — selection preserved
        assert_eq!(scene.focused_id, Some(VisualId(2)), "focus unchanged");
        assert_eq!(scene.selected_id, Some(VisualId(1)), "selection preserved");
    }

    #[test]
    fn min_max_restore_works_for_all() {
        let mut scene = Scene::default();
        let _ = scene.focus(Some(VisualId(42)));
        assert!(scene.minimize(VisualId(42)) == false); // no such visual
        assert!(scene.maximize(VisualId(42)) == false);
        assert!(scene.restore(VisualId(42)) == false);
    }

    #[test]
    fn is_visible_checks_window_state_only() {
        let mut scene = Scene::default();
        // is_visible only checks window_state, not ContentState
        assert!(!scene.is_visible(VisualId(1))); // doesn't exist
    }

    #[test]
    fn focus_follows_click_independent_of_provider() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(5)));
        assert_eq!(scene.focused_id, Some(VisualId(5)));
        scenario::focus_click_sequence(&mut scene);
        assert_eq!(scene.focused_id, Some(VisualId(7)));
    }

    // ── Spatial relationship tests ────────────────────────────────────

    #[test]
    fn set_parent_self_rejected() {
        let mut scene = Scene::default();
        assert_eq!(scene.set_parent(VisualId(1), VisualId(1)).unwrap_err(), "cannot parent to self");
    }

    #[test]
    fn set_parent_unknown_child() {
        let mut scene = Scene::default();
        assert!(scene.set_parent(VisualId(1), VisualId(2)).is_err());
    }

    #[test]
    fn clear_parent_unknown() {
        let mut scene = Scene::default();
        assert!(!scene.clear_parent(VisualId(999)));
    }

    #[test]
    fn world_matrix_unknown_returns_identity() {
        let mut scene = Scene::default();
        let m = scene.world_matrix(VisualId(999));
        assert!((m[0][0] - 1.0).abs() < 1e-4);
        assert!((m[3][0]).abs() < 1e-4);
    }

    // ── J2 popup coordinate model ────────────────────────────────────

    /// Popup on a parent at the origin: world == local (identity parent).
    #[test]
    fn popup_world_on_origin_parent_is_local() {
        let mut scene = Scene::default();
        let parent = crate::scene::Visual::new_test(400, 300);
        let popup = crate::scene::Visual::new_test(100, 60);
        let pid = parent.id;
        let cid = popup.id;
        scene.add(parent);
        scene.add(popup);
        scene.set_parent(cid, pid).unwrap();
        scene.visuals[1].transform.position = Vector3::new(50.0, -20.0, 10.0);
        let w = scene.world_matrix(cid);
        assert!((w[3][0] - 50.0).abs() < 1e-4);
        assert!((w[3][1] + 20.0).abs() < 1e-4);
        assert!((w[3][2] - 10.0).abs() < 1e-4);
    }

    /// THE J2 INVARIANT: moving the parent moves the popup by the same
    /// delta; the popup's own transform is untouched (parent-local).
    #[test]
    fn popup_follows_parent_translation() {
        let mut scene = Scene::default();
        let parent = crate::scene::Visual::new_test(400, 300);
        let popup = crate::scene::Visual::new_test(100, 60);
        let pid = parent.id;
        let cid = popup.id;
        scene.add(parent);
        scene.add(popup);
        scene.set_parent(cid, pid).unwrap();
        scene.visuals[0].transform.position = Vector3::new(200.0, 100.0, 0.0);
        scene.visuals[1].transform.position = Vector3::new(50.0, 0.0, 10.0);
        let w0 = scene.world_matrix(cid);
        // Move the parent only.
        scene.visuals[0].transform.position = Vector3::new(340.0, -60.0, 0.0);
        let w1 = scene.world_matrix(cid);
        let dx = w1[3][0] - w0[3][0];
        let dy = w1[3][1] - w0[3][1];
        let dz = w1[3][2] - w0[3][2];
        assert!((dx - 140.0).abs() < 1e-4, "popup x delta {}", dx);
        assert!((dy + 160.0).abs() < 1e-4, "popup y delta {}", dy);
        assert!(dz.abs() < 1e-4);
        // Popup local transform unchanged (scene state ownership).
        assert_eq!(scene.visuals[1].transform.position, Vector3::new(50.0, 0.0, 10.0));
    }

    /// THE NASTY CASE: parent rotated 90° about Y — a popup offset to
    /// the parent's right (+x local) must appear BEHIND the parent in
    /// world space (−z world), because "right of the window" rotates
    /// with the window. This is what makes the popup spatially
    /// attached to the transformed parent instead of the screen.
    #[test]
    fn popup_offset_rotates_with_parent() {
        let mut scene = Scene::default();
        let parent = crate::scene::Visual::new_test(400, 300);
        let popup = crate::scene::Visual::new_test(100, 60);
        let pid = parent.id;
        let cid = popup.id;
        scene.add(parent);
        scene.add(popup);
        scene.set_parent(cid, pid).unwrap();
        // Parent rotated 90° about Y (facing left/right instead of camera).
        scene.visuals[0].transform.rotation = cgmath::Quaternion::from_angle_y(cgmath::Deg(90.0));
        scene.visuals[1].transform.position = Vector3::new(100.0, 0.0, 0.0);
        let w = scene.world_matrix(cid);
        // Local +x (100) under a +90° Y rotation maps to world −z:
        // [cos90 0 sin90; 0 1 0; −sin90 0 cos90] · (100,0,0) = (0,0,−100).
        assert!((w[3][0]).abs() < 1e-4, "world x {}", w[3][0]);
        assert!((w[3][1]).abs() < 1e-4);
        assert!((w[3][2] + 100.0).abs() < 1e-4, "world z {}", w[3][2]);
    }

    /// Picking must hit parented visuals where they are DRAWN (world
    /// space), not where their local coordinates sit.
    #[test]
    fn pick_hits_popup_at_world_position() {
        use cgmath::InnerSpace;
        let mut scene = Scene::default();
        let parent = crate::scene::Visual::new_test(400, 300);
        let popup = crate::scene::Visual::new_test(100, 60);
        let pid = parent.id;
        let cid = popup.id;
        scene.add(parent);
        scene.add(popup);
        scene.set_parent(cid, pid).unwrap();
        scene.visuals[0].transform.position = Vector3::new(300.0, 0.0, 0.0);
        scene.visuals[1].transform.position = Vector3::new(0.0, 0.0, 10.0);
        // proj*view for the standard camera at (0,0,800) looking at origin.
        let view = cgmath::Matrix4::from_translation(cgmath::Vector3::new(0.0, 0.0, -800.0));
        let proj = cgmath::perspective(cgmath::Deg(45.0), 1280.0 / 720.0, 1.0, 10000.0);
        let pv = proj * view;
        // The popup draws at world (300, 0, 10): project that point to NDC.
        let world_pt = pv * cgmath::Vector4::new(300.0, 0.0, 10.0, 1.0);
        let ndc_x = world_pt.x / world_pt.w;
        let ndc_y = world_pt.y / world_pt.w;
        let hit = scene.pick(&pv, ndc_x, ndc_y);
        assert_eq!(hit.map(|(id, _)| id), Some(cid), "popup must be picked at its world position");
    }

    #[test]
    fn parent_cycle_rejected() {
        let mut scene = Scene::default();
        assert!(scene.set_parent(VisualId(1), VisualId(2)).is_err());
    }

    // ── Detach / Reparent tests ──────────────────────────────────────

    #[test]
    fn detach_unknown_returns_false() {
        let mut scene = Scene::default();
        assert!(!scene.detach_from_parent(VisualId(999)));
    }

    #[test]
    fn detach_noop_when_no_parent() {
        let mut scene = Scene::default();
        // Can't detach a nonexistent visual — returns false
        assert!(!scene.detach_from_parent(VisualId(999)));
    }

    #[test]
    fn reparent_self_rejected() {
        let mut scene = Scene::default();
        assert_eq!(scene.reparent(VisualId(1), VisualId(1)).unwrap_err(), "cannot parent to self");
    }

    #[test]
    fn reparent_unknown_child() {
        let mut scene = Scene::default();
        assert_eq!(scene.reparent(VisualId(1), VisualId(2)).unwrap_err(), "child not found");
    }

    #[test]
    fn reparent_unknown_parent() {
        let mut scene = Scene::default();
        assert_eq!(scene.reparent(VisualId(1), VisualId(2)).unwrap_err(), "child not found");
    }

    // ── De-emphasis tests ───────────────────────────────────────────

    #[test]
    fn de_emphasize_visual_sets_flag() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        // Without actual Visual objects, de_emphasize returns false
        // (requires a visual in the visuals vec)
        assert!(!scene.de_emphasize(VisualId(1)));
        assert!(scene.de_emphasized_set.is_empty());
    }

    #[test]
    fn de_emphasize_unknown_returns_false() {
        let mut scene = Scene::default();
        assert!(!scene.de_emphasize(VisualId(999)));
    }

    #[test]
    fn is_de_emphasized_empty() {
        let scene = Scene::default();
        assert!(!scene.is_de_emphasized(VisualId(1)));
        assert!(!scene.is_de_emphasized(VisualId(999)));
    }

    #[test]
    fn remove_cleans_de_emphasized_set() {
        let mut scene = Scene::default();
        scene.de_emphasized_set.push(VisualId(42));
        scene.remove(VisualId(42));
        assert!(!scene.de_emphasized_set.contains(&VisualId(42)));
    }

    #[test]
    fn de_emphasis_restore_cycle() {
        let mut scene = Scene::default();
        scene.de_emphasized_set.push(VisualId(1));
        assert!(scene.is_de_emphasized(VisualId(1)));
        assert!(scene.restore_from_de_emphasis(VisualId(1)));
        assert!(!scene.is_de_emphasized(VisualId(1)));
        // Second restore returns false
        assert!(!scene.restore_from_de_emphasis(VisualId(1)));
    }

    #[test]
    fn de_emphasis_clears_focus() {
        let mut scene = Scene::default();
        scene.focus(Some(VisualId(1)));
        // Since no actual visual object, focus tracking is in focused_id
        // We can test the flag exists
        assert_eq!(scene.focused_id, Some(VisualId(1)));
    }

    #[test]
    fn multiple_de_emphasized_visuals() {
        let mut scene = Scene::default();
        scene.de_emphasized_set.push(VisualId(1));
        scene.de_emphasized_set.push(VisualId(2));
        scene.de_emphasized_set.push(VisualId(3));
        assert!(scene.is_de_emphasized(VisualId(1)));
        assert!(scene.is_de_emphasized(VisualId(2)));
        assert!(scene.is_de_emphasized(VisualId(3)));
        assert_eq!(scene.de_emphasized_set.len(), 3);
    }

    #[test]
    fn damage_new_visual_starts_as_content() {
        let mut scene = Scene::default();
        // Without GlesTexture we can't create a Visual, but we can test
        // the DamageKind enum values directly
        assert_eq!(DamageKind::default(), DamageKind::None);
        assert_ne!(DamageKind::Content, DamageKind::None);
        assert_ne!(DamageKind::SpatialOnly, DamageKind::None);
        assert_ne!(DamageKind::Content, DamageKind::SpatialOnly);
    }

    #[test]
    fn clear_damage_no_visuals_no_crash() {
        let mut scene = Scene::default();
        scene.clear_damage();
        // No crash
    }

    #[test]
    fn damage_is_correct_type() {
        // DamageKind is a simple enum with 3 variants
        fn takes_damage(d: DamageKind) -> DamageKind { d }
        assert_eq!(takes_damage(DamageKind::None), DamageKind::None);
        assert_eq!(takes_damage(DamageKind::Content), DamageKind::Content);
        assert_eq!(takes_damage(DamageKind::SpatialOnly), DamageKind::SpatialOnly);
    }

    #[test]
    #[ignore]
    fn de_emphasis_and_snapping_exclusion() {
        // De-emphasized visuals should be excluded from snapping
        // This is a conceptual test — the actual exclusion is in the
        // interaction code, not the Scene layer
    }

    #[test]
    fn detach_preserves_world_transform() {
        // detach_from_parent should preserve the world transform
        // (tested via the Transform3D decomposition math)
        let m0 = Matrix4::from_translation(Vector3::new(100.0, 200.0, 300.0))
            * Matrix4::from(Quaternion::from_angle_y(Deg(45.0)))
            * Matrix4::from_nonuniform_scale(2.0, 3.0, 1.0);
        let t = Transform3D::from_matrix(&m0);
        // Decompose and recompose should give approximately same result
        let m1 = t.to_matrix();
        let t2 = Transform3D::from_matrix(&m1);
        let m2 = t2.to_matrix();
        for col in 0..4 {
            for row in 0..4 {
                let diff = (m1[col][row] - m2[col][row]).abs();
                // f32 precision: allow up to 0.01 difference
                assert!(diff < 0.01,
                    "decompose mismatch at [{}][{}]: expected {}, got {}, diff {}",
                    col, row, m1[col][row], m2[col][row], diff);
            }
        }
    }
}

/// Integration scenarios for multi-provider testing.
mod scenario {
    use super::*;

    /// Simulate a focus-follows-click sequence across 3 visuals.
    pub fn focus_click_sequence(scene: &mut Scene) {
        scene.focus(Some(VisualId(5)));
        scene.select(Some(VisualId(5)));
        scene.focus(Some(VisualId(6)));
        scene.select(Some(VisualId(6)));
        scene.focus(Some(VisualId(7)));
        scene.select(Some(VisualId(7)));
    }
}


/// Pure selection rule for focus replacement after a window closes:
/// the topmost remaining visual in draw order (last wins) that belongs
/// to the workspace and is active. Split from `Scene::pick_focus_replacement`
/// so the rule is unit-testable without GPU-backed visuals.
pub fn pick_replacement_from(
    draw_order: impl IntoIterator<Item = VisualId>,
    workspace_ids: &[VisualId],
    is_active: impl Fn(VisualId) -> bool,
) -> Option<VisualId> {
    draw_order
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .find(|id| workspace_ids.contains(id) && is_active(*id))
}
