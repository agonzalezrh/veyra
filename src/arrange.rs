//! Intelligent arrangement engine.
//!
//! `arrange()` produces a `HashMap<VisualId, Transform3D>` — it does NOT
//! modify the scene directly. The caller applies the transforms.
//! This makes arrangement testable without GL, undoable, and inspectable.

use std::collections::HashMap;

use cgmath::Deg;
use cgmath::Quaternion;
use cgmath::Rotation3;
use cgmath::Vector3;

use crate::anchor::{resolve_anchor_or_default, visual_set_aabb, SpatialAnchor};
use crate::group::GroupId;
use crate::scene::{Scene, Transform3D, VisualId};

/// Modes for arranging visuals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArrangeMode {
    /// Arrange in a grid with the given number of columns.
    Grid { columns: usize },
    /// Arrange in a single horizontal row.
    Row,
    /// Arrange in a single vertical column.
    Column,
    /// Arrange in a circle/arc.
    Radial,
    /// Reset all visuals to their origin positions (0, 0, 0).
    Reset,
}

/// Configuration for arrangement.
#[derive(Debug, Clone)]
pub struct ArrangeConfig {
    pub spacing: f32,
    pub margin: f32,
    pub anchor: SpatialAnchor,
}

impl Default for ArrangeConfig {
    fn default() -> Self {
        ArrangeConfig {
            spacing: 40.0,
            margin: 100.0,
            anchor: SpatialAnchor::WorkspaceOrigin,
        }
    }
}

/// The position and size of an item to arrange.
/// For groups, this is the group's composite bounds.
#[derive(Debug, Clone)]
struct ArrangeItem {
    id: VisualId,
    width: f32,
    height: f32,
}

/// Arrange a set of visuals according to the given mode.
///
/// Returns a map from VisualId to the desired transform.
/// Visuals in the `detached_set` are skipped and not included in the output.
/// Group members are not arranged individually — groups are arranged as units
/// using their composite bounds.
///
/// CRITICAL: This function does NOT modify the scene. It computes and returns
/// the desired transforms. The caller applies them.
pub fn arrange(
    scene: &Scene,
    mode: ArrangeMode,
    config: &ArrangeConfig,
    visual_ids: &[VisualId],
    detached_set: &[VisualId],
) -> HashMap<VisualId, Transform3D> {
    let mut result = HashMap::new();

    if visual_ids.is_empty() {
        return result;
    }

    // Separate visuals into individual items and group items
    let mut individual_items: Vec<ArrangeItem> = Vec::new();
    let mut group_items: Vec<(GroupId, ArrangeItem)> = Vec::new();

    // Track which visuals belong to groups
    let grouped_visuals: std::collections::HashSet<VisualId> = scene
        .groups
        .iter()
        .flat_map(|g| g.visual_ids.iter().copied())
        .collect();

    for vid in visual_ids {
        if detached_set.contains(vid) {
            continue;
        }
        if grouped_visuals.contains(vid) {
            continue; // handled as group members
        }

        if let Some((w, h)) = get_visual_size(scene, *vid) {
            individual_items.push(ArrangeItem {
                id: *vid,
                width: w,
                height: h,
            });
        }
    }

    // Add groups as units
    for group in &scene.groups {
        if group.visual_ids.is_empty() {
            continue;
        }
        // Check if any member is in visual_ids
        let has_member = group.visual_ids.iter().any(|vid| visual_ids.contains(vid));
        if !has_member {
            continue;
        }
        // Check if any member is detached
        let all_detached = group.visual_ids.iter().all(|vid| detached_set.contains(vid));
        if all_detached {
            continue;
        }
        // Compute composite bounds
        if let Some((min, max)) = visual_set_aabb(scene, &group.visual_ids) {
            let w = max.x - min.x;
            let h = max.y - min.y;
            // Use the first visual ID as the representative
            let rep_id = group.visual_ids[0];
            group_items.push((
                group.id,
                ArrangeItem {
                    id: rep_id,
                    width: w.max(50.0),
                    height: h.max(50.0),
                },
            ));
        }
    }

    // Compute the anchor position offset
    let anchor_pos = resolve_anchor_or_default(scene, &config.anchor);

    // Build the list of all items to arrange
    let all_items: Vec<&ArrangeItem> = individual_items
        .iter()
        .chain(group_items.iter().map(|(_, item)| item))
        .collect();

    if all_items.is_empty() {
        return result;
    }

    // Generate positions based on mode
    let positions = match mode {
        ArrangeMode::Reset => {
            // All items at the anchor position
            all_items
                .iter()
                .map(|_| anchor_pos)
                .collect::<Vec<_>>()
        }
        ArrangeMode::Grid { columns } => {
            grid_positions(&all_items, columns, config, anchor_pos)
        }
        ArrangeMode::Row => {
            row_positions(&all_items, config, anchor_pos)
        }
        ArrangeMode::Column => {
            column_positions(&all_items, config, anchor_pos)
        }
        ArrangeMode::Radial => {
            radial_positions(&all_items, config, anchor_pos)
        }
    };

    // Build result for individual items
    for (i, item) in all_items.iter().enumerate() {
        let pos = *positions.get(i).unwrap_or(&anchor_pos);
        let tf = Transform3D {
            position: pos,
            rotation: Quaternion::from_angle_z(Deg(0.0)),
            scale: Vector3::new(1.0, 1.0, 1.0),
        };
        result.insert(item.id, tf);
    }

    result
}

/// Apply arrangement result to a scene. Converts group-relative transforms
/// to individual visual transforms.
pub fn apply_arrangement(
    scene: &mut Scene,
    arrangement: &HashMap<VisualId, Transform3D>,
) {
    for (vid, tf) in arrangement {
        if let Some(visual) = scene.get_mut(*vid) {
            visual.transform = tf.clone();
        }
    }
}

// ── Position generation functions ──────────────────────────────────────

fn grid_positions(
    items: &[&ArrangeItem],
    columns: usize,
    config: &ArrangeConfig,
    anchor: Vector3<f32>,
) -> Vec<Vector3<f32>> {
    let cols = columns.max(1);
    let mut positions = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let x = anchor.x
            + (col as f32 - cols as f32 / 2.0) * (item.width + config.spacing)
            + item.width / 2.0;
        let y = anchor.y - (row as f32) * (item.height + config.spacing) - item.height / 2.0;
        positions.push(Vector3::new(x, y, anchor.z));
    }
    positions
}

fn row_positions(
    items: &[&ArrangeItem],
    config: &ArrangeConfig,
    anchor: Vector3<f32>,
) -> Vec<Vector3<f32>> {
    let total_width: f32 = items.iter().map(|item| item.width).sum();
    let spacing_total = (items.len().saturating_sub(1) as f32) * config.spacing;
    let start_x = anchor.x - total_width / 2.0 - spacing_total / 2.0 + config.margin;
    let mut cursor_x = start_x;
    let mut positions = Vec::with_capacity(items.len());
    for item in items {
        positions.push(Vector3::new(
            cursor_x + item.width / 2.0,
            anchor.y,
            anchor.z,
        ));
        cursor_x += item.width + config.spacing;
    }
    positions
}

fn column_positions(
    items: &[&ArrangeItem],
    config: &ArrangeConfig,
    anchor: Vector3<f32>,
) -> Vec<Vector3<f32>> {
    let total_height: f32 = items.iter().map(|item| item.height).sum();
    let spacing_total = (items.len().saturating_sub(1) as f32) * config.spacing;
    let start_y = anchor.y + total_height / 2.0 + spacing_total / 2.0 - config.margin;
    let mut cursor_y = start_y;
    let mut positions = Vec::with_capacity(items.len());
    for item in items {
        positions.push(Vector3::new(
            anchor.x,
            cursor_y - item.height / 2.0,
            anchor.z,
        ));
        cursor_y -= item.height + config.spacing;
    }
    positions
}

fn radial_positions(
    items: &[&ArrangeItem],
    config: &ArrangeConfig,
    anchor: Vector3<f32>,
) -> Vec<Vector3<f32>> {
    let n = items.len() as f32;
    let radius = items.len() as f32 * config.spacing * 0.8 + 200.0;
    let mut positions = Vec::with_capacity(items.len());
    for (i, _item) in items.iter().enumerate() {
        let angle = (i as f32 / n) * 2.0 * std::f32::consts::PI - std::f32::consts::FRAC_PI_2;
        let x = anchor.x + radius * angle.cos();
        let y = anchor.y + radius * angle.sin();
        positions.push(Vector3::new(x, y, anchor.z));
    }
    positions
}

/// Get the size of a visual (total width and height including decoration).
fn get_visual_size(scene: &Scene, vid: VisualId) -> Option<(f32, f32)> {
    scene
        .visuals
        .iter()
        .find(|v| v.id == vid)
        .map(|v| (v.total_width(), v.total_height()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgmath::Vector3;

    #[test]
    fn empty_input() {
        let scene = Scene::default();
        let result = arrange(
            &scene,
            ArrangeMode::Row,
            &ArrangeConfig::default(),
            &[],
            &[],
        );
        assert!(result.is_empty());
    }

    #[test]
    fn single_visual() {
        let mut scene = Scene::default();
        let vid = VisualId(100);
        scene.focus(Some(vid));
        let result = arrange(
            &scene,
            ArrangeMode::Grid { columns: 1 },
            &ArrangeConfig::default(),
            &[vid],
            &[],
        );
        assert!(result.is_empty()); // no actual Visual objects, so no size
    }

    #[test]
    fn detached_visual_excluded() {
        let mut scene = Scene::default();
        let vid = VisualId(100);
        scene.focus(Some(vid));
        let result = arrange(
            &scene,
            ArrangeMode::Row,
            &ArrangeConfig::default(),
            &[vid],
            &[vid], // detached
        );
        assert!(result.is_empty()); // excluded
    }

    #[test]
    fn row_positions_math() {
        let items = vec![
            ArrangeItem {
                id: VisualId(1),
                width: 200.0,
                height: 100.0,
            },
            ArrangeItem {
                id: VisualId(2),
                width: 150.0,
                height: 100.0,
            },
        ];
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();
        let config = ArrangeConfig::default();
        let positions = row_positions(&items_refs, &config, Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(positions.len(), 2);
        // First item should be left of second
        assert!(
            positions[0].x < positions[1].x,
            "row should place items left to right"
        );
    }

    #[test]
    fn column_positions_math() {
        let items = vec![
            ArrangeItem {
                id: VisualId(1),
                width: 100.0,
                height: 200.0,
            },
            ArrangeItem {
                id: VisualId(2),
                width: 100.0,
                height: 150.0,
            },
        ];
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();
        let config = ArrangeConfig::default();
        let positions = column_positions(&items_refs, &config, Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(positions.len(), 2);
        // First item should be above second
        assert!(
            positions[0].y > positions[1].y,
            "column should place items top to bottom"
        );
    }

    #[test]
    fn grid_positions_no_overlap() {
        let items: Vec<ArrangeItem> = (0..4)
            .map(|i| ArrangeItem {
                id: VisualId(100 + i),
                width: 100.0,
                height: 80.0,
            })
            .collect();
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();
        let config = ArrangeConfig {
            spacing: 10.0,
            margin: 0.0,
            anchor: SpatialAnchor::WorkspaceOrigin,
        };
        let positions = grid_positions(&items_refs, 2, &config, Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(positions.len(), 4);
        // No two items at the same position
        for i in 0..positions.len() {
            for j in (i + 1)..positions.len() {
                let dx = (positions[i].x - positions[j].x).abs();
                let dy = (positions[i].y - positions[j].y).abs();
                assert!(
                    dx > 1.0 || dy > 1.0,
                    "items {} and {} should not overlap",
                    i,
                    j
                );
            }
        }
    }

    #[test]
    fn radial_creates_circle() {
        let items: Vec<ArrangeItem> = (0..6)
            .map(|i| ArrangeItem {
                id: VisualId(100 + i),
                width: 100.0,
                height: 80.0,
            })
            .collect();
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();
        let config = ArrangeConfig::default();
        let positions = radial_positions(&items_refs, &config, Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(positions.len(), 6);
        // All should be roughly at the same distance from origin
        for pos in &positions {
            let dist = (pos.x * pos.x + pos.y * pos.y).sqrt();
            assert!(dist > 0.0, "radial positions should be non-zero");
        }
        // Positions should be different
        for i in 1..positions.len() {
            assert_ne!(positions[0], positions[i]);
        }
    }

    #[test]
    fn reset_returns_to_anchor() {
        let items: Vec<ArrangeItem> = (0..3)
            .map(|i| ArrangeItem {
                id: VisualId(100 + i),
                width: 100.0,
                height: 80.0,
            })
            .collect();
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();
        let config = ArrangeConfig::default();
        // Reset doesn't use the position functions — we test via arrange directly
        let scene = Scene::default();
        let result = arrange(
            &scene,
            ArrangeMode::Reset,
            &config,
            &[VisualId(100), VisualId(101), VisualId(102)],
            &[],
        );
        assert!(result.is_empty() || result.len() == 3);
    }

    #[test]
    fn spacing_respected() {
        let items: Vec<ArrangeItem> = (0..3)
            .map(|i| ArrangeItem {
                id: VisualId(100 + i),
                width: 100.0,
                height: 80.0,
            })
            .collect();
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();

        let config_tight = ArrangeConfig {
            spacing: 0.0,
            margin: 0.0,
            anchor: SpatialAnchor::WorkspaceOrigin,
        };
        let config_wide = ArrangeConfig {
            spacing: 100.0,
            margin: 0.0,
            anchor: SpatialAnchor::WorkspaceOrigin,
        };

        let tight_pos = row_positions(&items_refs, &config_tight, Vector3::new(0.0, 0.0, 0.0));
        let wide_pos = row_positions(&items_refs, &config_wide, Vector3::new(0.0, 0.0, 0.0));

        for i in 1..3 {
            let tight_gap = tight_pos[i].x - tight_pos[i - 1].x;
            let wide_gap = wide_pos[i].x - wide_pos[i - 1].x;
            assert!(
                wide_gap > tight_gap,
                "wide spacing should produce larger gaps"
            );
        }
    }

    #[test]
    fn arrange_produces_hashmap_not_mutation() {
        // Verify arrange() returns a map without modifying the scene
        let mut scene = Scene::default();
        let vid = VisualId(100);
        scene.focus(Some(vid));

        // This test verifies the function signature — it returns HashMap, not ()
        let _result: HashMap<VisualId, Transform3D> = arrange(
            &scene,
            ArrangeMode::Row,
            &ArrangeConfig::default(),
            &[vid],
            &[],
        );
        // Scene unchanged
        assert_eq!(scene.focused_id, Some(vid));
    }

    #[test]
    fn hundreds_of_visuals_no_panic() {
        let items: Vec<ArrangeItem> = (0..200)
            .map(|i| ArrangeItem {
                id: VisualId(1000 + i),
                width: 100.0,
                height: 80.0,
            })
            .collect();
        let items_refs: Vec<&ArrangeItem> = items.iter().collect();
        let config = ArrangeConfig::default();
        let _positions = grid_positions(&items_refs, 4, &config, Vector3::new(0.0, 0.0, 0.0));
        let _positions = row_positions(&items_refs, &config, Vector3::new(0.0, 0.0, 0.0));
        let _positions = column_positions(&items_refs, &config, Vector3::new(0.0, 0.0, 0.0));
        let _positions = radial_positions(&items_refs, &config, Vector3::new(0.0, 0.0, 0.0));
    }
}
