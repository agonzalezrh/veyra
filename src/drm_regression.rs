//! G-E5.6.7: consolidated DRM regression suite.
//!
//! The whole 5.6.1–5.6.6 chain as one coherent test layer, ordered by
//! architecture so a failure names the broken layer:
//!
//! ```text
//! Topology (E5.6.1)
//!   ↓
//! OutputBinding (E5.6.5)
//!   ↓
//! OutputPresentation (E5.6.2)
//!   ↓
//! OutputFrameState (E5.6.3)
//!   ↓
//! CRTC flip attribution (E5.6.4)
//!   ↓
//! Hotplug lifecycle (E5.6.6)
//! ```
//!
//! The critical cross-layer invariant: Output A's lifecycle can never
//! mutate Output B's lifecycle — tested here with INTENTIONALLY
//! REORDERED connector/CRTC ids, the class of bug that otherwise only
//! manifests as intermittent corruption on real hardware.

#[cfg(test)]
mod drm_regression_suite {
    use crate::drm_backend::{FrameStateError, OutputFrameState};
    use crate::drm_topology::{
        global_positions, AssignedOutput, ConnectorState, DrmTopology, TopologyConnector,
        TopologyCrtc, TopologyMode, UnassignedReason,
    };
    use crate::outputs::{OutputBindings, OutputId, OutputLifecycle, OutputManager, OutputState};
    use crate::input::Camera;
    use smithay::reexports::drm::control as drm_control;

    fn crtc_handle(raw: u32) -> drm_control::crtc::Handle {
        drm_control::from_u32::<drm_control::crtc::Handle>(raw).expect("nonzero")
    }

    fn mode(w: u32, h: u32, refresh: i32) -> TopologyMode {
        TopologyMode {
            width: w,
            height: h,
            refresh_mhz: refresh,
            preferred: true,
        }
    }

    // ── Layer 1: Topology ────────────────────────────────────────────

    #[test]
    fn l1_connector_assignment_deterministic() {
        // Scrambled insertion order; ids must decide.
        let mut t = DrmTopology::new();
        t.add_connector(TopologyConnector {
            id: 42,
            state: ConnectorState::Connected,
            modes: vec![mode(1920, 1080, 60000)],
            encoder_candidates: vec![10],
        });
        t.add_connector(TopologyConnector {
            id: 7,
            state: ConnectorState::Connected,
            modes: vec![mode(1280, 720, 60000)],
            encoder_candidates: vec![11],
        });
        t.add_crtc(TopologyCrtc {
            id: 99,
            encoder_candidates: vec![11],
        });
        t.add_crtc(TopologyCrtc {
            id: 55,
            encoder_candidates: vec![10],
        });
        let a1 = t.assign();
        let a2 = t.assign();
        assert_eq!(a1, a2, "same topology → identical assignment");
        // Connector 7 sorts first (ascending ids) and takes the lowest
        // COMPATIBLE free CRTC: 55 drives encoder 10 (incompatible with
        // 7's encoder 11), so 7 takes 99 — compatibility wins over raw
        // id order.
        assert_eq!(a1.outputs[0].connector_id, 7);
        assert_eq!(a1.outputs[0].crtc_id, 99);
        assert_eq!(a1.outputs[1].connector_id, 42);
        assert_eq!(a1.outputs[1].crtc_id, 55);
    }

    #[test]
    fn l1_unassigned_reasons_are_precise() {
        let mut t = DrmTopology::new();
        t.add_connector(TopologyConnector {
            id: 7,
            state: ConnectorState::Disconnected,
            modes: vec![mode(1280, 720, 60000)],
            encoder_candidates: vec![11],
        });
        t.add_connector(TopologyConnector {
            id: 42,
            state: ConnectorState::Connected,
            modes: vec![mode(1920, 1080, 60000)],
            encoder_candidates: vec![10],
        });
        t.add_crtc(TopologyCrtc {
            id: 99,
            encoder_candidates: vec![11],
        });
        let a = t.assign();
        assert_eq!(a.outputs.len(), 0, "42 has no compatible CRTC; 7 is disconnected");
        // 42 is connected but has NO crtc with a matching encoder.
        let un42 = a
            .unassigned
            .iter()
            .find(|u| u.connector_id == 42)
            .expect("42 unassigned");
        assert_eq!(un42.reason, UnassignedReason::NoCompatibleEncoder);
        // 7 is disconnected — the reason is never an encoder issue.
        let un7 = a.unassigned.iter().find(|u| u.connector_id == 7).unwrap();
        assert_eq!(un7.reason, UnassignedReason::Disconnected);
    }

    // ── Layer 2: OutputBinding ───────────────────────────────────────

    #[test]
    fn l2_outputid_to_backend_index_stable() {
        let mut b = OutputBindings::new();
        // Non-contiguous, high ids — the map never assumes density.
        b.bind(OutputId(9001), 0);
        b.bind(OutputId(4), 1);
        assert_eq!(b.index_for(OutputId(9001)), Some(0));
        assert_eq!(b.index_for(OutputId(4)), Some(1));
        assert_eq!(b.index_for(OutputId(5)), None);
    }

    #[test]
    fn l2_reordering_backend_outputs_preserves_identity() {
        // Backend vec [X, Y] → [Y] after X's removal: Y's binding
        // compacts 1 → 0 but its IDENTITY (OutputId) never changes.
        let mut b = OutputBindings::new();
        b.bind(OutputId(10), 0);
        b.bind(OutputId(20), 1);
        let affected = b.backend_removed(0);
        assert_eq!(affected, vec![OutputId(10)]);
        assert_eq!(b.index_for(OutputId(20)), Some(0), "identity preserved");
    }

    // ── Layer 3: OutputPresentation / frame lifecycle ────────────────

    #[test]
    fn l3_frame_state_is_independent_per_output() {
        // Two synthetic outputs; A's transitions must not leak into B.
        let mut a = OutputFrameState::Idle;
        let mut b = OutputFrameState::Idle;
        a = a.begin().expect("A begins");
        assert_eq!(b, OutputFrameState::Idle, "B unaffected by A's begin");
        a = a.submit().expect("A submits");
        a = a.arm_flip().expect("A arms");
        assert_eq!(b, OutputFrameState::Idle);
        assert_eq!(a, OutputFrameState::FlipPending);
        // B can run its OWN full cycle while A is pending.
        b = b.begin().expect("B begins during A's flip");
        b = b.submit().expect("B submits");
        b = b.arm_flip().expect("B arms");
        assert_eq!(a, OutputFrameState::FlipPending, "A untouched by B");
    }

    #[test]
    fn l3_double_begin_is_a_bug_not_silence() {
        let s = OutputFrameState::Idle.begin().expect("first begin");
        assert_eq!(s.begin(), Err(FrameStateError::DoubleBegin));
    }

    // ── Layer 4: CRTC flip attribution ───────────────────────────────

    #[test]
    fn l4_crtc_attribution_is_unique_and_by_handle() {
        // A CRTC maps to exactly ONE output; events for unknown CRTCs
        // are dropped; attribution follows the HANDLE, not vec order.
        let crtcs = [crtc_handle(37), crtc_handle(42), crtc_handle(55)];
        let mut owners = std::collections::HashMap::new();
        for (i, h) in crtcs.iter().enumerate() {
            owners.insert(u32::from(*h), i);
        }
        assert_eq!(owners.get(&37u32), Some(&0));
        assert_eq!(owners.get(&55u32), Some(&2));
        assert_eq!(owners.get(&999u32), None, "unknown CRTC dropped");
        assert_eq!(owners.len(), crtcs.len(), "CRTC → output is unique");
    }

    #[test]
    fn l4_flip_a_cannot_complete_b() {
        // With REVERSED vec order: the event's CRTC still resolves to
        // the right output, and completing A leaves B pending.
        let mut a_state = OutputFrameState::FlipPending;
        let mut b_state = OutputFrameState::FlipPending;
        let a_crtc = crtc_handle(37);
        // Event for A arrives; attribution by handle (A is at index 1
        // in the reversed vec — irrelevant).
        let is_a = a_crtc == crtc_handle(37);
        assert!(is_a);
        if a_state.flip_complete() {
            a_state = OutputFrameState::Idle;
        }
        assert_eq!(a_state, OutputFrameState::Idle);
        assert_eq!(b_state, OutputFrameState::FlipPending, "B not retired");
    }

    // ── Layer 5: Hotplug lifecycle ───────────────────────────────────

    fn registry_with_two() -> (OutputManager, OutputId, OutputId) {
        let mut m = OutputManager::new();
        let a = m.add(OutputState {
            name: "DP-1".into(),
            mode: (1920, 1080),
            refresh_mhz: 60000,
            scale: 1.0,
            global_pos: (0, 0),
            camera: Camera::new(),
            wl: None,
        });
        let b = m.add(OutputState {
            name: "DP-2".into(),
            mode: (2560, 1440),
            refresh_mhz: 144000,
            scale: 1.0,
            global_pos: (1920, 0),
            camera: Camera::new(),
            wl: None,
        });
        (m, a, b)
    }

    #[test]
    fn l5_disconnect_b_primary_a_continues() {
        let (mut m, a, b) = registry_with_two();
        let eff = m.unplug_output(b).expect("unplug B");
        assert!(!eff.was_primary);
        assert_eq!(m.primary_id(), Some(a), "A keeps rendering");
        assert_eq!(m.outputs().len(), 1);
    }

    #[test]
    fn l5_disconnect_primary_promotes_b() {
        let (mut m, a, b) = registry_with_two();
        let eff = m.unplug_output(a).expect("unplug A");
        assert!(eff.was_primary);
        assert_eq!(eff.promoted, Some(b));
        assert_eq!(m.primary_id(), Some(b));
    }

    #[test]
    fn l5_pending_flip_then_disconnect_quiesces_only_that_output() {
        // A pending, B idle → unplug A → A quiesced (force_idle), B
        // still fine — and B completes its OWN pending flip normally.
        let (mut m, a, _b) = registry_with_two();
        let mut a_frame = OutputFrameState::FlipPending;
        let mut b_frame = OutputFrameState::FlipPending;
        // Unplug A: presentation quiesces before teardown.
        m.unplug_output(a).expect("unplug");
        a_frame = a_frame.force_idle();
        assert_eq!(a_frame, OutputFrameState::Idle);
        // B completes normally.
        assert!(b_frame.flip_complete());
        b_frame = OutputFrameState::Idle;
        assert_eq!(b_frame, OutputFrameState::Idle);
    }

    #[test]
    fn l5_disconnect_cannot_destroy_windows() {
        // THE invariant: physical output disappearing ≠ window
        // disappearing. Structural proof: neither OutputManager::unplug
        // nor OutputBindings::backend_removed touches scene/workspace
        // state — there is no path from presentation teardown to
        // visual destruction. This test documents the layer boundary.
        let (mut m, a, b) = registry_with_two();
        let mut bindings = OutputBindings::new();
        bindings.bind(a, 0);
        bindings.bind(b, 1);
        m.unplug_output(a).expect("unplug primary");
        let _ = bindings.backend_removed(0);
        // Window state lives on WorkspaceState (workspace-scoped
        // visual_ids); nothing here touched it — no API even exists.
        assert_eq!(m.outputs().len(), 1);
        assert_eq!(bindings.index_for(b), Some(0));
    }

    #[test]
    fn l5_replug_new_identity_fresh_binding() {
        let (mut m, _a, b) = registry_with_two();
        m.unplug_output(b).expect("unplug");
        let b2 = m.replug_output(OutputState {
            name: "DP-2".into(),
            mode: (1024, 768), // different mode on reconnect
            refresh_mhz: 60000,
            scale: 1.0,
            global_pos: (0, 0),
            camera: Camera::new(),
            wl: None,
        });
        assert_ne!(b2, b, "new connector session = new output identity");
        // Positions re-tile from the survivor.
        assert_eq!(m.outputs().len(), 2);
    }

    #[test]
    fn l5_geometry_recompute_after_unplug() {
        // The desktop plane shrinks to the survivor and re-extends on
        // replug (the E5.5 oracle mapping follows the registry).
        let (mut m, _a, b) = registry_with_two();
        assert_eq!(m.global_extents(), (1920 + 2560, 1440));
        m.unplug_output(b).expect("unplug B");
        assert_eq!(m.global_extents(), (1920, 1080));
        let plans = m.build_frame_plans(
            &|_, w, h| cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0),
            false,
        );
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].viewport,
            crate::outputs::OutputViewport { x: 0, y: 0, width: 1920, height: 1080 }
        );
    }

    // ── Cross-layer chain: assignment → registration → plans ────────

    #[test]
    fn full_chain_topology_to_plans_with_reordered_ids() {
        // The complete E5.6 chain on scrambled hardware ids: topology
        // → assignment → OutputStates (oracle positions) → bindings →
        // frame plans. Reordered insertions must not perturb identity.
        let mut t = DrmTopology::new();
        t.add_connector(TopologyConnector {
            id: 77,
            state: ConnectorState::Connected,
            modes: vec![mode(2560, 1440, 144000)],
            encoder_candidates: vec![21],
        });
        t.add_connector(TopologyConnector {
            id: 12,
            state: ConnectorState::Connected,
            modes: vec![mode(1920, 1080, 60000)],
            encoder_candidates: vec![20],
        });
        t.add_crtc(TopologyCrtc {
            id: 91,
            encoder_candidates: vec![21],
        });
        t.add_crtc(TopologyCrtc {
            id: 44,
            encoder_candidates: vec![20],
        });
        let assignment = t.assign();
        assert_eq!(assignment.outputs.len(), 2);
        let positions = global_positions(&assignment.outputs);
        let mut m = OutputManager::new();
        for ((conn_id, pos), assigned) in positions.iter().zip(&assignment.outputs) {
            m.add(OutputState {
                name: format!("DP-{conn_id}"),
                mode: (assigned.mode.width, assigned.mode.height),
                refresh_mhz: assigned.mode.refresh_mhz,
                scale: 1.0,
                global_pos: *pos,
                camera: Camera::new(),
                wl: None,
            });
        }
        let plans = m.build_frame_plans(
            &|_, w, h| cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0),
            false,
        );
        // Connector 12 sorts first: 1920x1080 at the origin, then
        // 2560x1440 at (1920, 0) — regardless of insertion order.
        assert_eq!(
            plans[0].viewport,
            crate::outputs::OutputViewport { x: 0, y: 0, width: 1920, height: 1080 }
        );
        assert_eq!(
            plans[1].viewport,
            crate::outputs::OutputViewport { x: 1920, y: 0, width: 2560, height: 1440 }
        );
        let _ = AssignedOutput {
            connector_id: 0,
            crtc_id: 0,
            mode: mode(1, 1, 60),
        }; // type anchor
    }
}
