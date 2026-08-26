use std::time::Instant;

const LOG_INTERVAL: u64 = 120;

#[derive(Debug, Clone)]
pub struct PerfStats {
    pub frame_count: u64,
    pub last_log: Instant,

    // Per-stage accumulators (reset every LOG_INTERVAL)
    pub stage_update_ns: u64,      // FrameProducer.update()
    pub stage_tex_copy_ns: u64,    // Copy new texture to Visual
    pub stage_remove_ns: u64,      // Remove finished Visuals
    pub stage_render_bind_ns: u64, // Renderer bind + GlesFrame creation
    pub stage_render_draw_ns: u64, // Scene traversal + GLES draw calls
    pub stage_render_submit_ns: u64, // Swap/present
    pub stage_total_ns: u64,       // Total frame
    pub frame_count_since_log: u64,

    pub consecutive_drops: u64,
    pub total_drops: u64,

    // Instrumentation counters (reset every LOG_INTERVAL)
    pub frame_requested: u64,      // schedule_render() called
    pub frame_rendered: u64,       // render() actually rendered
    pub frame_presented: u64,      // eglSwapBuffers succeeded
    pub frame_dropped: u64,        // render() skipped (idle)
    pub damage_frames: u64,        // frames with real content change
    pub idle_frames: u64,          // consecutive idle frames
}

impl PerfStats {
    pub fn new() -> Self {
        PerfStats {
            frame_count: 0,
            last_log: Instant::now(),
            stage_update_ns: 0,
            stage_tex_copy_ns: 0,
            stage_remove_ns: 0,
            stage_render_bind_ns: 0,
            stage_render_draw_ns: 0,
            stage_render_submit_ns: 0,
            stage_total_ns: 0,
            frame_count_since_log: 0,
            consecutive_drops: 0,
            total_drops: 0,
            frame_requested: 0,
            frame_rendered: 0,
            frame_presented: 0,
            frame_dropped: 0,
            damage_frames: 0,
            idle_frames: 0,
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame_count += 1;
    }

    pub fn record_stage(&mut self, stage: PipelineStage, elapsed_ns: u64) {
        match stage {
            PipelineStage::ProducerUpdate => self.stage_update_ns += elapsed_ns,
            PipelineStage::TexCopy => self.stage_tex_copy_ns += elapsed_ns,
            PipelineStage::Remove => self.stage_remove_ns += elapsed_ns,
            PipelineStage::RenderBind => self.stage_render_bind_ns += elapsed_ns,
            PipelineStage::RenderDraw => self.stage_render_draw_ns += elapsed_ns,
            PipelineStage::RenderSubmit => self.stage_render_submit_ns += elapsed_ns,
            PipelineStage::Total => self.stage_total_ns += elapsed_ns,
        }
    }

    pub fn record_requested(&mut self) {
        self.frame_requested += 1;
    }

    pub fn record_dropped(&mut self) {
        self.consecutive_drops += 1;
        self.total_drops += 1;
        self.frame_dropped += 1;
    }

    pub fn record_rendered(&mut self) {
        self.frame_rendered += 1;
    }

    pub fn record_presented(&mut self) {
        self.frame_presented += 1;
    }

    pub fn record_damage(&mut self) {
        self.damage_frames += 1;
    }

    pub fn record_idle(&mut self) {
        self.idle_frames += 1;
    }

    pub fn record_frame(&mut self) {
        self.consecutive_drops = 0;
        self.frame_count_since_log += 1;
        if self.frame_count_since_log >= LOG_INTERVAL {
            self.log();
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.frame_count_since_log = 0;
        self.stage_update_ns = 0;
        self.stage_tex_copy_ns = 0;
        self.stage_remove_ns = 0;
        self.stage_render_bind_ns = 0;
        self.stage_render_draw_ns = 0;
        self.stage_render_submit_ns = 0;
        self.stage_total_ns = 0;
        self.frame_requested = 0;
        self.frame_rendered = 0;
        self.frame_presented = 0;
        self.frame_dropped = 0;
        self.damage_frames = 0;
        self.idle_frames = 0;
        self.last_log = Instant::now();
    }

    fn log(&self) {
        let n = self.frame_count_since_log.max(1);
        let avg_total = self.stage_total_ns / n;
        let avg_update = self.stage_update_ns / n;
        let avg_tex = self.stage_tex_copy_ns / n;
        let avg_remove = self.stage_remove_ns / n;
        let avg_bind = self.stage_render_bind_ns / n;
        let avg_draw = self.stage_render_draw_ns / n;
        let avg_submit = self.stage_render_submit_ns / n;

        let avg_fps = 1_000_000_000.0 / avg_total.max(1) as f64;
        let to_ms = |ns: u64| format!("{:.3}", ns as f64 / 1_000_000.0);

        tracing::info!(
            frames = %n,
            total = %self.frame_count,
            fps = format!("{:.1}", avg_fps),
            total_ms = to_ms(avg_total),
            update_ms = to_ms(avg_update),
            tex_ms = to_ms(avg_tex),
            remove_ms = to_ms(avg_remove),
            bind_ms = to_ms(avg_bind),
            draw_ms = to_ms(avg_draw),
            submit_ms = to_ms(avg_submit),
            drops = %self.total_drops,
            requested = %self.frame_requested,
            rendered = %self.frame_rendered,
            presented = %self.frame_presented,
            dropped_stats = %self.frame_dropped,
            damage = %self.damage_frames,
            idle = %self.idle_frames,
            "PROFILE"
        );
    }
}

pub enum PipelineStage {
    ProducerUpdate,
    TexCopy,
    Remove,
    RenderBind,
    RenderDraw,
    RenderSubmit,
    Total,
}
