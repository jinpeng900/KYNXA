use crate::sampling::HardwareSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkFeedback {
    pub throughput_per_second: Option<f64>,
    pub latency_ms: Option<f64>,
    pub queue_depth: Option<u32>,
    pub allocation_failure: Option<bool>,
    pub foreground_latency_ms: Option<f64>,
    pub progress: Option<f64>,
    pub phase: Option<String>,
    pub unit: Option<String>,
    pub backend: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptivePolicy {
    pub background_fraction: f64,
    pub throughput_per_second: Option<f64>,
    pub latency_ms: Option<f64>,
    pub queue_depth: Option<u32>,
    pub last_adjustment_ms: u64,
    pub reports: u64,
    pub task_backend_fraction: f64,
    pub adjustment_reason: &'static str,
    pub measurement_context: Option<String>,
    #[serde(skip)]
    histories: HashMap<String, (Option<f64>, Option<f64>, u64)>,
    #[serde(skip)]
    fractions: HashMap<String, (f64, u64)>,
}

impl AdaptivePolicy {
    pub fn new() -> Self {
        Self {
            background_fraction: 1.0,
            throughput_per_second: None,
            latency_ms: None,
            queue_depth: None,
            last_adjustment_ms: 0,
            reports: 0,
            task_backend_fraction: 1.0,
            adjustment_reason: "not-adjusted",
            measurement_context: None,
            histories: HashMap::new(),
            fractions: HashMap::new(),
        }
    }

    pub fn report(&mut self, task_id: &str, feedback: WorkFeedback, now_ms: u64) -> bool {
        if [
            feedback.throughput_per_second,
            feedback.latency_ms,
            feedback.foreground_latency_ms,
            feedback.progress,
        ]
        .into_iter()
        .flatten()
        .any(|value| !value.is_finite() || value < 0.0 || value > 1.0e9)
            || feedback.queue_depth.is_some_and(|depth| depth > 1_000_000)
            || feedback.progress.is_some_and(|value| value > 1.0)
            || feedback.phase.as_ref().is_some_and(|value| !["cold-load","queue","hot-inference","other"].contains(&value.as_str()))
            || feedback.unit.as_ref().is_some_and(|value| !["tokens","documents","pairs","vectors","operations"].contains(&value.as_str()))
            || feedback.backend.as_ref().is_some_and(|value| !["cpu","gpu","dml","cuda","host"].contains(&value.as_str()))
        {
            return false;
        }
        let phase = feedback.phase.as_deref().unwrap_or("legacy");
        let backend = feedback.backend.as_deref().unwrap_or("legacy");
        let backend_group = if ["gpu","dml","cuda"].contains(&backend) {"gpu"} else {backend};
        let context = format!("{}|{}|{}|{}", task_id, backend, phase, feedback.unit.as_deref().unwrap_or("legacy"));
        let control_context = if backend == "legacy" { "legacy".to_string() } else { format!("{}|{}",task_id,backend_group) };
        if self.histories.len() >= 128 && !self.histories.contains_key(&context) {
            if let Some(oldest) = self
                .histories
                .iter()
                .min_by_key(|(_, history)| history.2)
                .map(|(key, _)| key.clone())
            {
                self.histories.remove(&oldest);
            }
        }
        let history = self
            .histories
            .entry(context.clone())
            .or_insert((None, None, now_ms));
        if self.fractions.len() >= 128 && !self.fractions.contains_key(&control_context) {
            if let Some(oldest) = self.fractions.iter().min_by_key(|(_, value)|value.1).map(|(key,_)|key.clone()) {
                self.fractions.remove(&oldest);
            }
        }
        let control = self.fractions.entry(control_context).or_insert((1.0, 0));
        let is_pressure = feedback.allocation_failure == Some(true)
            || feedback
                .foreground_latency_ms
                .is_some_and(|latency| latency > 250.0);
        let is_gain = feedback
            .throughput_per_second
            .zip(history.0)
            .is_some_and(|(current, previous)| current > previous * 1.05)
            && feedback
                .latency_ms
                .zip(history.1)
                .is_none_or(|(current, previous)| current <= previous * 1.1)
            && feedback.queue_depth.is_some_and(|depth| depth > 0);
        // Pressure shrinks immediately; growth needs measured benefit and a hold interval.
        // 压力立即减半；扩张必须有实测吞吐收益且满足保持时间，缺少指标不按零处理。
        if is_pressure {
            control.0 = (control.0 / 2.0).max(0.125);
            control.1 = now_ms;
            self.adjustment_reason = if feedback.allocation_failure == Some(true) {"allocation-pressure"} else {"foreground-latency"};
            self.last_adjustment_ms = now_ms;
        } else if is_gain && ["legacy","hot-inference"].contains(&phase) && now_ms.saturating_sub(control.1) >= 5000 {
            control.0 = (control.0 + 0.125).min(1.0);
            control.1 = now_ms;
            self.adjustment_reason = "measured-hot-throughput-gain";
            self.last_adjustment_ms = now_ms;
        } else {
            self.adjustment_reason = if !["legacy","hot-inference"].contains(&phase) {"non-hot-sample-held"}
                else if is_gain {"hold-interval"} else {"no-comparable-throughput-gain"};
        }
        // Explicit backend feedback controls its own task/backend only; legacy callers retain their old default.
        // 显式后端反馈只影响对应任务与后端；旧调用方保留原默认，冷热和不同吞吐单位不混比。
        if backend == "legacy" { self.background_fraction = control.0; }
        self.task_backend_fraction = control.0;
        self.measurement_context = Some(context);
        if let Some(value) = feedback.throughput_per_second {
            history.0 = Some(
                history
                    .0
                    .map_or(value, |previous| previous * 0.75 + value * 0.25),
            );
        }
        if let Some(value) = feedback.latency_ms {
            history.1 = Some(
                history
                    .1
                    .map_or(value, |previous| previous * 0.75 + value * 0.25),
            );
        }
        history.2 = now_ms;
        self.throughput_per_second = history.0;
        self.latency_ms = history.1;
        if feedback.queue_depth.is_some() {
            self.queue_depth = feedback.queue_depth;
        }
        self.reports += 1;
        true
    }

    pub fn fraction(&self, task_id: &str, uses_gpu: bool) -> f64 {
        self.fractions.get(&format!("{}|{}",task_id,if uses_gpu {"gpu"} else {"cpu"}))
            .map_or(self.background_fraction,|value|value.0)
    }

    pub fn suggestions(
        &self,
        hardware: &HardwareSnapshot,
        granted_cpu: usize,
        granted_memory: u64,
        task_id: &str,
        uses_gpu: bool,
    ) -> Value {
        let factor = self.fraction(task_id,uses_gpu);
        let usable_memory = granted_memory.min(hardware.memory.available_bytes / 3);
        let pressure = hardware.cpu.usage_percent.is_some_and(|usage| usage > 85.0);
        let candidates = ((if pressure {
            40.0
        } else {
            40.0 + hardware.cpu.logical_cores.min(32) as f64 * 3.0
        }) * factor)
            .round()
            .clamp(16.0, 136.0) as u32;
        json!({ "annShardBytes": (usable_memory / 2).min(1024 * 1024 * 1024),
            "annCacheBytes": usable_memory, "annBuildConcurrency": granted_cpu.min(4).max(1),
            "candidateLimit": candidates, "fusedCandidateLimit": (candidates * 6 / 5).min(160),
            "evidenceBudgetTokens": (candidates * 128).clamp(2048, 16384),
            "batchMultiplier": factor,"adjustmentReason":if factor < 1.0 {"held-task-backend-pressure"} else {"capacity-approved"},
            "lastFeedbackReason":self.adjustment_reason,"measurementContext":self.measurement_context,
            "source": "resource-authority" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aimd_requires_measured_gain_and_waits_after_pressure() {
        let mut policy = AdaptivePolicy::new();
        assert!(policy.report(
            "embedding",
            WorkFeedback {
                allocation_failure: Some(true),
                ..Default::default()
            },
            100
        ));
        assert_eq!(policy.background_fraction, 0.5);
        policy.report(
            "embedding",
            WorkFeedback {
                throughput_per_second: Some(10.0),
                latency_ms: Some(10.0),
                queue_depth: Some(10),
                ..Default::default()
            },
            1000,
        );
        policy.report(
            "embedding",
            WorkFeedback {
                throughput_per_second: Some(20.0),
                latency_ms: Some(9.0),
                queue_depth: Some(10),
                ..Default::default()
            },
            2000,
        );
        assert_eq!(policy.background_fraction, 0.5);
        policy.report(
            "different-unit",
            WorkFeedback {
                throughput_per_second: Some(10000.0),
                latency_ms: Some(9.0),
                queue_depth: Some(10),
                ..Default::default()
            },
            6100,
        );
        assert_eq!(policy.background_fraction, 0.5);
        policy.report(
            "embedding",
            WorkFeedback {
                throughput_per_second: Some(30.0),
                latency_ms: Some(9.0),
                queue_depth: Some(10),
                ..Default::default()
            },
            6200,
        );
        assert_eq!(policy.background_fraction, 0.625);
        assert!(!policy.report(
            "embedding",
            WorkFeedback {
                throughput_per_second: Some(f64::NAN),
                ..Default::default()
            },
            7000
        ));
    }

    #[test]
    fn device_units_and_cold_load_are_not_compared_as_hot_throughput() {
        let mut policy = AdaptivePolicy::new();
        let feedback = |phase:&str,unit:&str,backend:&str,throughput:f64|WorkFeedback {
            phase:Some(phase.into()),unit:Some(unit.into()),backend:Some(backend.into()),
            throughput_per_second:Some(throughput),latency_ms:Some(10.0),queue_depth:Some(5),..Default::default()
        };
        policy.report("embedding",WorkFeedback {allocation_failure:Some(true),backend:Some("dml".into()),..Default::default()},100);
        assert_eq!(policy.fraction("embedding",true),0.5);
        assert_eq!(policy.fraction("embedding",false),1.0);
        policy.report("embedding",feedback("cold-load","documents","dml",10.0),1000);
        policy.report("embedding",feedback("cold-load","documents","dml",100.0),10000);
        assert_eq!(policy.fraction("embedding",true),0.5);
        assert_eq!(policy.adjustment_reason,"non-hot-sample-held");
        policy.report("embedding",feedback("hot-inference","tokens","dml",100000.0),11000);
        assert_eq!(policy.fraction("embedding",true),0.5);
        policy.report("embedding",feedback("hot-inference","tokens","dml",150000.0),12000);
        assert_eq!(policy.fraction("embedding",true),0.625);
        assert_eq!(policy.fraction("embedding",false),1.0);
        assert_eq!(policy.adjustment_reason,"measured-hot-throughput-gain");
    }
}
