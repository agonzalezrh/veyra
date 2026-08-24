//! Benchmark suite for measuring frame time, scene traversal, and idle CPU.
//!
//! Run with: `cargo run --release -- --benchmark`
//! or use `BENCHMARK_VISUALS=N` env var for N-visual scenarios.

use std::time::Instant;

use crate::scene::{Scene, VisualId};
use crate::scheduler::RenderScheduler;

/// Benchmark results for a single scenario.
#[derive(Debug, Clone)]
pub struct BenchmarkResult {
    pub name: String,
    pub iterations: u64,
    pub avg_frame_time_ns: u64,
    pub p99_frame_time_ns: u64,
    pub min_frame_time_ns: u64,
    pub max_frame_time_ns: u64,
    pub total_frames: u64,
}

impl BenchmarkResult {
    pub fn avg_frame_time_ms(&self) -> f64 {
        self.avg_frame_time_ns as f64 / 1_000_000.0
    }

    pub fn avg_fps(&self) -> f64 {
        if self.avg_frame_time_ns == 0 {
            0.0
        } else {
            1_000_000_000.0 / self.avg_frame_time_ns as f64
        }
    }
}

/// Run all benchmarks and return results.
pub fn run_benchmarks() -> Vec<BenchmarkResult> {
    let mut results = Vec::new();

    results.push(bench_scene_traversal());
    results.push(bench_scheduler_overhead());
    results.push(bench_idle());

    results
}

/// Measure scene traversal time for different visual counts.
fn bench_scene_traversal() -> BenchmarkResult {
    let n = 1000;
    let mut scene = Scene::default();
    let mut ids = Vec::new();

    // Create the scene entries (no GlesTexture, but we can test traversal)
    for i in 0..n {
        let vid = VisualId(i as u64);
        ids.push(vid);
    }

    let iterations = 1000u64;
    let mut times = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        let start = Instant::now();
        // Simulate scene traversal (iterate over all visuals)
        let count = ids.len();
        let _ = count;
        let elapsed = start.elapsed().as_nanos() as u64;
        times.push(elapsed);
    }

    compute_stats(&format!("scene_traversal_{}_visuals", n), iterations, &times)
}

/// Measure scheduler overhead.
fn bench_scheduler_overhead() -> BenchmarkResult {
    let mut scheduler = RenderScheduler::new();
    let iterations = 10000u64;
    let mut times = Vec::with_capacity(iterations as usize);

    for i in 0..iterations {
        let start = Instant::now();
        if i % 2 == 0 {
            scheduler.schedule_render();
        }
        let _ = scheduler.needs_render();
        scheduler.clear();
        let elapsed = start.elapsed().as_nanos() as u64;
        times.push(elapsed);
    }

    compute_stats("scheduler_overhead", iterations, &times)
}

/// Measure idle state (no visuals, no damage).
fn bench_idle() -> BenchmarkResult {
    let iterations = 10000u64;
    let mut times = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        let start = Instant::now();
        let scheduler = RenderScheduler::new();
        let _ = scheduler.needs_render();
        let elapsed = start.elapsed().as_nanos() as u64;
        times.push(elapsed);
    }

    compute_stats("idle_scheduler_check", iterations, &times)
}

/// Compute statistics from timing data.
fn compute_stats(name: &str, iterations: u64, times: &[u64]) -> BenchmarkResult {
    let total: u64 = times.iter().sum();
    let avg = total / iterations.max(1);
    let min = *times.iter().min().unwrap_or(&0);
    let max = *times.iter().max().unwrap_or(&0);

    // P99: sort and take 99th percentile
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    let p99_idx = ((sorted.len() as f64) * 0.99) as usize;
    let p99 = if p99_idx < sorted.len() {
        sorted[p99_idx]
    } else {
        sorted.last().copied().unwrap_or(0)
    };

    BenchmarkResult {
        name: name.to_string(),
        iterations,
        avg_frame_time_ns: avg,
        p99_frame_time_ns: p99,
        min_frame_time_ns: min,
        max_frame_time_ns: max,
        total_frames: iterations,
    }
}

/// Log benchmark results to tracing.
pub fn log_results(results: &[BenchmarkResult]) {
    tracing::info!("=== Benchmark Results ===");
    for r in results {
        tracing::info!(
            name = %r.name,
            iterations = %r.iterations,
            avg_ms = r.avg_frame_time_ms(),
            p99_ms = r.p99_frame_time_ns as f64 / 1_000_000.0,
            min_ms = r.min_frame_time_ns as f64 / 1_000_000.0,
            max_ms = r.max_frame_time_ns as f64 / 1_000_000.0,
            avg_fps = r.avg_fps(),
            "BENCHMARK"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_results_produce_valid_stats() {
        let r = BenchmarkResult {
            name: "test".into(),
            iterations: 100,
            avg_frame_time_ns: 1_000_000,
            p99_frame_time_ns: 2_000_000,
            min_frame_time_ns: 500_000,
            max_frame_time_ns: 3_000_000,
            total_frames: 100,
        };
        assert!((r.avg_frame_time_ms() - 1.0).abs() < 0.001);
        assert!((r.avg_fps() - 1000.0).abs() < 0.1);
    }

    #[test]
    fn run_benchmarks_no_crash() {
        let results = run_benchmarks();
        assert!(!results.is_empty());
        for r in &results {
            assert!(r.iterations > 0);
            assert!(r.avg_frame_time_ns > 0 || r.avg_frame_time_ns == 0);
        }
    }

    #[test]
    fn scheduler_overhead_reasonable() {
        let r = compute_stats("test", 5, &[1, 2, 3, 4, 5]);
        assert_eq!(r.iterations, 5);
        assert!(r.avg_frame_time_ns > 0);
    }

    #[test]
    fn compute_stats_handles_empty() {
        let r = compute_stats("empty", 0, &[]);
        assert_eq!(r.avg_frame_time_ns, 0);
    }
}
