//! G-E5.6.1: DRM topology model — a pure representation of a DRM
//! device's connector/CRTC structure and the deterministic assignment
//! of connectors to CRTCs. Exercised by unit tests now, wired to the
//! real device in G-E5.6.2 (hence the dead-code allowance until then).
//!
//! `DrmGraphicsBackend` must stop implicitly meaning "the first
//! connected display". This module owns topology DISCOVERY and
//! ASSIGNMENT as pure logic; the real-device mapping (drm-rs types →
//! these structs) and the per-output GBM presentation state land in
//! G-E5.6.2/6.3. No rendering changes here.

#![allow(dead_code)]

//! Key semantics:
//! - A connector can drive a CRTC only if their encoder candidate sets
//!   OVERLAP (KMS constraint: connector → encoder → CRTC).
//! - Assignment is deterministic: connected connectors are considered
//!   in ascending connector-id order; each takes the lowest-numbered
//!   compatible free CRTC. The same hardware always produces the same
//!   assignment.
//! - A connected connector with no compatible free CRTC stays
//!   UNASSIGNED (recorded with a reason) — it becomes an output only
//!   when a CRTC frees up (re-assignment runs on every topology event).
//! - Mode selection: the connector's PREFERRED mode if any, else the
//!   highest pixel-area mode, tie-broken by refresh.

/// Connection state as reported by KMS (drmModeConnector.connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorState {
    Connected,
    Disconnected,
    Unknown,
}

/// One display mode (physical pixels + refresh in mHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopologyMode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: i32,
    /// KMS "preferred" flag from the connector's EDID.
    pub preferred: bool,
}

impl TopologyMode {
    /// Pixel area for fallback ordering.
    fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// One KMS connector: id, connection state, its modes, and the
/// encoders it can drive (the KMS encoder-id candidates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyConnector {
    pub id: u32,
    pub state: ConnectorState,
    pub modes: Vec<TopologyMode>,
    pub encoder_candidates: Vec<u32>,
}

impl TopologyConnector {
    /// G-E5.6.1: the mode this connector should present at — the
    /// PREFERRED mode if the EDID declares one (first preferred wins),
    /// else the largest area, tie-broken by highest refresh.
    pub fn preferred_mode(&self) -> Option<TopologyMode> {
        if let Some(m) = self.modes.iter().find(|m| m.preferred) {
            return Some(*m);
        }
        self.modes
            .iter()
            .copied()
            .max_by(|a, b| {
                a.area()
                    .cmp(&b.area())
                    .then(a.refresh_mhz.cmp(&b.refresh_mhz))
            })
    }
}

/// One KRTC — a CRTC with the set of encoders it can be driven by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyCrtc {
    pub id: u32,
    pub encoder_candidates: Vec<u32>,
}

/// The result of assigning one connector to a CRTC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedOutput {
    pub connector_id: u32,
    pub crtc_id: u32,
    pub mode: TopologyMode,
}

/// A connector that could not be assigned, with the reason (lifecycle
/// events may change this later — G-E5.6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnassignedReason {
    Disconnected,
    NoCompatibleEncoder,
    NoFreeCrtc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnassignedConnector {
    pub connector_id: u32,
    pub reason: UnassignedReason,
}

/// A complete topology assignment for one device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopologyAssignment {
    pub outputs: Vec<AssignedOutput>,
    pub unassigned: Vec<UnassignedConnector>,
}

/// The pure device topology: connectors and CRTCs as discovered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DrmTopology {
    pub connectors: Vec<TopologyConnector>,
    pub crtcs: Vec<TopologyCrtc>,
}

impl DrmTopology {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_connector(&mut self, c: TopologyConnector) {
        self.connectors.push(c);
        // Deterministic discovery order everywhere downstream.
        self.connectors.sort_by_key(|c| c.id);
    }

    pub fn add_crtc(&mut self, c: TopologyCrtc) {
        self.crtcs.push(c);
        self.crtcs.sort_by_key(|c| c.id);
    }

    /// G-E5.6.1: deterministically assign connected connectors to
    /// compatible free CRTCs.
    ///
    /// Compatible = the connector's and CRTC's encoder candidate sets
    /// overlap (a KMS encoder can bridge them). Free = no earlier
    /// assignment claimed it in THIS pass (first-come per ascending
    /// connector id). The result is a pure function of the topology:
    /// identical hardware state always assigns identically.
    pub fn assign(&self) -> TopologyAssignment {
        let mut assignment = TopologyAssignment::default();
        let mut used_crtcs: Vec<u32> = Vec::new();

        for conn in &self.connectors {
            if conn.state != ConnectorState::Connected {
                assignment.unassigned.push(UnassignedConnector {
                    connector_id: conn.id,
                    reason: UnassignedReason::Disconnected,
                });
                continue;
            }
            // Deterministic mode choice.
            let Some(mode) = conn.preferred_mode() else {
                assignment.unassigned.push(UnassignedConnector {
                    connector_id: conn.id,
                    reason: UnassignedReason::NoCompatibleEncoder,
                });
                continue;
            };
            // Lowest-id CRTC whose encoder set overlaps AND is free.
            let crtc = self
                .crtcs
                .iter()
                .filter(|c| !used_crtcs.contains(&c.id))
                .find(|c| {
                    c.encoder_candidates
                        .iter()
                        .any(|e| conn.encoder_candidates.contains(e))
                });
            match crtc {
                Some(c) => {
                    used_crtcs.push(c.id);
                    assignment.outputs.push(AssignedOutput {
                        connector_id: conn.id,
                        crtc_id: c.id,
                        mode,
                    });
                }
                None => {
                    // Distinguish "no encoder overlap at all" from "all
                    // overlapping CRTCs are already claimed" — the
                    // lifecycle model treats them differently (G-E5.6.6:
                    // a hotplugged connector can be assigned later when
                    // a CRTC frees; an incompatible one never can).
                    let any_compatible = self.crtcs.iter().any(|c| {
                        c.encoder_candidates
                            .iter()
                            .any(|e| conn.encoder_candidates.contains(e))
                    });
                    assignment.unassigned.push(UnassignedConnector {
                        connector_id: conn.id,
                        reason: if any_compatible {
                            UnassignedReason::NoFreeCrtc
                        } else {
                            UnassignedReason::NoCompatibleEncoder
                        },
                    });
                }
            }
        }
        assignment
    }

    /// The number of connected connectors (upper bound on outputs).
    pub fn connected_count(&self) -> usize {
        self.connectors
            .iter()
            .filter(|c| c.state == ConnectorState::Connected)
            .count()
    }
}

/// G-E5.6.5: global desktop positions for a set of assigned outputs —
/// row tiling in ascending connector-id order, matching
/// OutputManager's default placement policy exactly (the simulation is
/// the oracle for these transforms).
pub fn global_positions(outputs: &[AssignedOutput]) -> Vec<(u32, (i32, i32))> {
    let mut x = 0i32;
    outputs
        .iter()
        .map(|o| {
            let pos = (x, 0);
            x += o.mode.width as i32;
            (o.connector_id, pos)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: u32, h: u32, refresh: i32, preferred: bool) -> TopologyMode {
        TopologyMode {
            width: w,
            height: h,
            refresh_mhz: refresh,
            preferred,
        }
    }

    fn connector(id: u32, encoders: &[u32], modes: Vec<TopologyMode>) -> TopologyConnector {
        TopologyConnector {
            id,
            state: ConnectorState::Connected,
            modes,
            encoder_candidates: encoders.to_vec(),
        }
    }

    fn crtc(id: u32, encoders: &[u32]) -> TopologyCrtc {
        TopologyCrtc {
            id,
            encoder_candidates: encoders.to_vec(),
        }
    }

    #[test]
    fn one_connector_one_crtc() {
        let mut t = DrmTopology::new();
        t.add_connector(connector(31, &[10], vec![mode(1920, 1080, 60000, true)]));
        t.add_crtc(crtc(50, &[10]));
        let a = t.assign();
        assert_eq!(a.outputs.len(), 1);
        assert_eq!(a.outputs[0].connector_id, 31);
        assert_eq!(a.outputs[0].crtc_id, 50);
        assert_eq!(a.outputs[0].mode.width, 1920);
        assert!(a.unassigned.is_empty());
    }

    #[test]
    fn two_connectors_two_crtcs() {
        let mut t = DrmTopology::new();
        t.add_connector(connector(31, &[10], vec![mode(1920, 1080, 60000, true)]));
        t.add_connector(connector(34, &[11], vec![mode(2560, 1440, 144000, true)]));
        t.add_crtc(crtc(50, &[10]));
        t.add_crtc(crtc(51, &[11]));
        let a = t.assign();
        assert_eq!(a.outputs.len(), 2);
        // Deterministic: lowest connector id first, lowest crtc id first.
        assert_eq!(a.outputs[0], AssignedOutput {
            connector_id: 31,
            crtc_id: 50,
            mode: mode(1920, 1080, 60000, true),
        });
        assert_eq!(a.outputs[1], AssignedOutput {
            connector_id: 34,
            crtc_id: 51,
            mode: mode(2560, 1440, 144000, true),
        });
    }

    #[test]
    fn two_connectors_one_crtc_second_unassigned() {
        let mut t = DrmTopology::new();
        t.add_connector(connector(31, &[10], vec![mode(1920, 1080, 60000, true)]));
        t.add_connector(connector(34, &[10], vec![mode(1920, 1080, 60000, true)]));
        t.add_crtc(crtc(50, &[10]));
        let a = t.assign();
        assert_eq!(a.outputs.len(), 1);
        assert_eq!(a.outputs[0].connector_id, 31, "lowest connector id wins");
        assert_eq!(a.unassigned.len(), 1);
        assert_eq!(a.unassigned[0].connector_id, 34);
        assert_eq!(a.unassigned[0].reason, UnassignedReason::NoFreeCrtc);
    }

    #[test]
    fn disconnected_connector_is_never_assigned() {
        let mut t = DrmTopology::new();
        let mut c = connector(31, &[10], vec![mode(1920, 1080, 60000, true)]);
        c.state = ConnectorState::Disconnected;
        t.add_connector(c);
        t.add_crtc(crtc(50, &[10]));
        let a = t.assign();
        assert!(a.outputs.is_empty());
        assert_eq!(a.unassigned[0].reason, UnassignedReason::Disconnected);
    }

    #[test]
    fn no_compatible_encoder_rejects_assignment() {
        let mut t = DrmTopology::new();
        t.add_connector(connector(31, &[10], vec![mode(1920, 1080, 60000, true)]));
        // CRTC only drives encoder 11 — no overlap with connector 31.
        t.add_crtc(crtc(50, &[11]));
        let a = t.assign();
        assert!(a.outputs.is_empty());
        assert_eq!(a.unassigned[0].reason, UnassignedReason::NoCompatibleEncoder);
    }

    #[test]
    fn preferred_mode_wins_over_larger() {
        let mut t = DrmTopology::new();
        // 4K not preferred; EDID prefers the smaller mode.
        t.add_connector(connector(
            31,
            &[10],
            vec![
                mode(3840, 2160, 60000, false),
                mode(1920, 1080, 144000, true),
            ],
        ));
        t.add_crtc(crtc(50, &[10]));
        let a = t.assign();
        assert_eq!(a.outputs[0].mode, mode(1920, 1080, 144000, true));
    }

    #[test]
    fn fallback_mode_is_largest_area_then_refresh() {
        let c = connector(
            31,
            &[10],
            vec![
                mode(1280, 720, 60000, false),
                mode(1920, 1080, 60000, false),
                mode(1920, 1080, 144000, false),
            ],
        );
        assert_eq!(
            c.preferred_mode(),
            Some(mode(1920, 1080, 144000, false)),
            "area tie → highest refresh"
        );
    }

    #[test]
    fn assignment_is_deterministic_across_runs() {
        let mut t = DrmTopology::new();
        t.add_connector(connector(34, &[10, 11], vec![mode(1920, 1080, 60000, true)]));
        t.add_connector(connector(31, &[10, 11], vec![mode(2560, 1440, 60000, true)]));
        t.add_crtc(crtc(51, &[10, 11]));
        t.add_crtc(crtc(50, &[10, 11]));
        let a1 = t.assign();
        let a2 = t.assign();
        assert_eq!(a1, a2, "same topology → identical assignment");
        // Connector 31 sorts first and claims crtc 50.
        assert_eq!(a1.outputs[0].connector_id, 31);
        assert_eq!(a1.outputs[0].crtc_id, 50);
        assert_eq!(a1.outputs[1].crtc_id, 51);
    }

    #[test]
    fn global_positions_tile_the_row() {
        let outputs = vec![
            AssignedOutput {
                connector_id: 31,
                crtc_id: 50,
                mode: mode(1920, 1080, 60000, true),
            },
            AssignedOutput {
                connector_id: 34,
                crtc_id: 51,
                mode: mode(2560, 1440, 60000, true),
            },
        ];
        let pos = global_positions(&outputs);
        assert_eq!(pos[0], (31, (0, 0)));
        assert_eq!(pos[1], (34, (1920, 0)));
        // Desktop spans 4480 px — the G-E5.5 oracle geometry.
        assert_eq!(1920 + 2560, 4480);
    }

    #[test]
    fn crtc_freedom_is_per_pass() {
        // Two connectors sharing one compatible CRTC: assignment order
        // (ascending connector id) decides; the loser records NoFreeCrtc
        // and becomes assignable when the CRTC frees (G-E5.6.6).
        // Connector 34 shares encoder 10 with CRTC 50 (taken by 31) —
        // CRTC 51 only drives encoder 11, so 34's reason is NoFreeCrtc
        // (drivable, but its only compatible CRTC is busy).
        let mut t = DrmTopology::new();
        t.add_connector(connector(31, &[10], vec![mode(1920, 1080, 60000, true)]));
        t.add_connector(connector(34, &[10], vec![mode(1920, 1080, 60000, true)]));
        t.add_crtc(crtc(50, &[10]));
        t.add_crtc(crtc(51, &[11]));
        let a = t.assign();
        assert_eq!(a.outputs.len(), 1);
        assert_eq!(a.outputs[0].crtc_id, 50);
        assert_eq!(
            a.unassigned[0].reason,
            UnassignedReason::NoFreeCrtc,
            "drivable via crtc 50's encoder — busy, not incompatible"
        );
    }

    #[test]
    fn fully_incompatible_connector_never_assignable() {
        // Connector 34 shares NO encoder with ANY crtc — NoCompatibleEncoder
        // even when all CRTCs are free.
        let mut t = DrmTopology::new();
        t.add_connector(connector(34, &[99], vec![mode(1920, 1080, 60000, true)]));
        t.add_crtc(crtc(50, &[10]));
        let a = t.assign();
        assert_eq!(a.unassigned[0].reason, UnassignedReason::NoCompatibleEncoder);
    }
}
