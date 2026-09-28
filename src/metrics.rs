//! Measurements: counters and delay percentiles per operation step.
//!
//! Series are per step, never per repository.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

const RESERVOIR: usize = 2048;

#[derive(Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<&'static str, u64>>,
    timings: Mutex<BTreeMap<&'static str, VecDeque<f64>>>,
}

impl Metrics {
    pub fn inc(&self, name: &'static str) {
        self.add(name, 1);
    }

    pub fn add(&self, name: &'static str, n: u64) {
        *self.counters.lock().unwrap().entry(name).or_default() += n;
    }

    pub fn get(&self, name: &str) -> u64 {
        self.counters
            .lock()
            .unwrap()
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    pub fn observe(&self, step: &'static str, d: Duration) {
        let mut t = self.timings.lock().unwrap();
        let q = t.entry(step).or_default();
        if q.len() == RESERVOIR {
            q.pop_front();
        }
        q.push_back(d.as_secs_f64());
    }

    /// Median, 95th and 99th percentile in seconds over recent samples.
    pub fn percentiles(&self, step: &str) -> Option<(f64, f64, f64, usize)> {
        let t = self.timings.lock().unwrap();
        let q = t.get(step)?;
        if q.is_empty() {
            return None;
        }
        let mut v: Vec<f64> = q.iter().copied().collect();
        v.sort_by(|a, b| a.total_cmp(b));
        let p = |x: f64| v[(((v.len() - 1) as f64) * x).round() as usize];
        Some((p(0.5), p(0.95), p(0.99), v.len()))
    }

    /// Prometheus text format.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (k, v) in self.counters.lock().unwrap().iter() {
            out.push_str(&format!("rustgit_{k}_total {v}\n"));
        }
        let steps: Vec<&'static str> = self.timings.lock().unwrap().keys().copied().collect();
        for step in steps {
            if let Some((p50, p95, p99, n)) = self.percentiles(step) {
                out.push_str(&format!(
                    "rustgit_step_seconds{{step=\"{step}\",quantile=\"0.5\"}} {p50}\n"
                ));
                out.push_str(&format!(
                    "rustgit_step_seconds{{step=\"{step}\",quantile=\"0.95\"}} {p95}\n"
                ));
                out.push_str(&format!(
                    "rustgit_step_seconds{{step=\"{step}\",quantile=\"0.99\"}} {p99}\n"
                ));
                out.push_str(&format!(
                    "rustgit_step_seconds_count{{step=\"{step}\"}} {n}\n"
                ));
            }
        }
        out
    }
}

/// Measures one step until dropped or finished.
pub struct StepTimer<'a> {
    metrics: &'a Metrics,
    step: &'static str,
    start: std::time::Instant,
}

impl<'a> StepTimer<'a> {
    pub fn new(metrics: &'a Metrics, step: &'static str) -> Self {
        Self {
            metrics,
            step,
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for StepTimer<'_> {
    fn drop(&mut self) {
        self.metrics.observe(self.step, self.start.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_recent_samples() {
        let m = Metrics::default();
        for i in 1..=100 {
            m.observe("cas", Duration::from_millis(i));
        }
        let (p50, p95, p99, n) = m.percentiles("cas").unwrap();
        assert_eq!(n, 100);
        assert!((p50 - 0.050).abs() < 0.002, "{p50}");
        assert!((p95 - 0.095).abs() < 0.002, "{p95}");
        assert!((p99 - 0.099).abs() < 0.002, "{p99}");
        assert!(
            m.render()
                .contains("rustgit_step_seconds{step=\"cas\",quantile=\"0.99\"}")
        );
    }
}
