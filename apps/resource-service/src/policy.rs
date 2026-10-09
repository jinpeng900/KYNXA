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
    pub input_tokens: Option<u32>,
    pub sequence_tokens: Option<u32>,
    pub batch_size: Option<u32>,
    pub cpu_threads: Option<u32>,
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
    histories: HashMap<String, (Option<f64>, Option<f64>, u64, Option<f64>)>,
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
            || feedback.input_tokens.is_some_and(|value| value == 0 || value > 65_536)
            || feedback.sequence_tokens.is_some_and(|value| value == 0 || value > 512)
            || feedback.batch_size.is_some_and(|value| value == 0 || value > 128)
            || feedback.cpu_threads.is_some_and(|value| value == 0 || value > 32)
        {
            return false;
        }
        let phase = feedback.phase.as_deref().unwrap_or("legacy");
        let backend = feedback.backend.as_deref().unwrap_or("legacy");
        let backend_group = if ["gpu","dml","cuda"].contains(&backend) {"gpu"} else {backend};
        let sequence_bucket = feedback.sequence_tokens.map(|value| value.next_power_of_two().to_string()).unwrap_or("legacy".into());
        let context = format!("{}|{}|{}|{}|{}", task_id, backend, phase, feedback.unit.as_deref().unwrap_or("legacy"), sequence_bucket);
        // Compare cost per token across batch sizes, while holding sequence-length classes apart.
        // 按每 token 成本跨批次比较，同时隔离不同序列长度类别。
        let comparable_latency_ms = feedback.latency_ms.map(|value| value / feedback.input_tokens.unwrap_or(1).max(1) as f64);
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
            .or_insert((None, None, now_ms, None));
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
                .latency_ms.map(|value| value / feedback.input_tokens.unwrap_or(1).max(1) as f64)
                .zip(history.1)
                .is_none_or(|(current, previous)| current <= previous * 1.1)
            && feedback.queue_depth.is_some_and(|depth| depth > 0);
        // Pressure shrinks immediately; growth needs measured benefit and a hold interval.
        // 压力立即收紧；扩张必须有实测吞吐收益且满足保持时间，缺少指标不按零处理。
        if is_pressure {
            control.0 = (control.0 / 2.0).min(0.5).max(0.125);
            control.1 = now_ms;
            self.adjustment_reason = if feedback.allocation_failure == Some(true) {"allocation-pressure"} else {"foreground-latency"};
            self.last_adjustment_ms = now_ms;
        } else if is_gain && ["legacy","hot-inference"].contains(&phase) && now_ms.saturating_sub(control.1) >= 5000 {
            control.0 = (if control.0 < 1.0 { control.0 + 0.125 } else { control.0 * 2.0 }).min(4.0);
            control.1 = now_ms;
            self.adjustment_reason = "measured-hot-throughput-gain";
            self.last_adjustment_ms = now_ms;
        } else if phase == "hot-inference" && feedback.queue_depth.is_some_and(|depth| depth > 0)
            && feedback.throughput_per_second.zip(history.0).is_some_and(|(current, previous)|current < previous * 0.9)
            && now_ms.saturating_sub(control.1) >= 5000 {
            control.0 = (control.0 / 2.0).max(0.125);
            control.1 = now_ms;
            self.adjustment_reason = "measured-hot-throughput-regression";
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
        if let Some(value) = comparable_latency_ms {
            history.1 = Some(
                history
                    .1
                    .map_or(value, |previous| previous * 0.75 + value * 0.25),
            );
        }
        if let Some(value) = feedback.latency_ms {
            history.3 = Some(history.3.map_or(value, |previous| previous * 0.75 + value * 0.25));
        }
        history.2 = now_ms;
        self.throughput_per_second = history.0;
        self.latency_ms = history.3;
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
        cpu_headroom: usize,
        memory_headroom_bytes: u64,
        gpu_headroom_bytes: u64,
    ) -> Value {
        let usable_memory = granted_memory.min(hardware.memory.available_bytes / 3);
        let pressure = hardware.cpu.usage_percent.is_some_and(|usage| usage > 85.0);
        let factor = if pressure || hardware.memory.available_bytes < 1024 * 1024 * 1024 {
            self.fraction(task_id,uses_gpu).min(0.5)
        } else { self.fraction(task_id,uses_gpu) };
        let is_idle = hardware.cpu.usage_percent.is_some_and(|usage| usage < 35.0)
            && hardware.memory.available_bytes >= 2 * 1024 * 1024 * 1024;
        // The search lease is planning overhead; suggestions do not approve reranker execution.
        // 检索租约仅为规划开销；建议本身不批准重排执行。
        let has_gpu_headroom = gpu_headroom_bytes >= 512 * 1024 * 1024;
        let rerank_candidates = if factor < 1.0 || pressure || memory_headroom_bytes < 512 * 1024 * 1024 || (cpu_headroom < 1 && !has_gpu_headroom) { 20 }
            else if (cpu_headroom >= 3 || has_gpu_headroom) && memory_headroom_bytes >= 1024 * 1024 * 1024 { 60 }
            else { 40 };
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
            "rerankCandidates":rerank_candidates,"rerankCandidateLimit":rerank_candidates,"batchMultiplier": factor,
            "rerankSuggestionRequiresApproval":true,
            "batchProbeMultiplier":if factor == 1.0 && is_idle {2.0} else {factor},
            "adjustmentReason":if factor < 1.0 {"held-task-backend-pressure"} else {"capacity-approved"},
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

    #[test]
    fn hot_gain_can_exceed_startup_but_pressure_and_length_classes_bound_exploration() {
        let mut policy = AdaptivePolicy::new();
        let sample = |tokens: u32, latency: f64, sequence: u32| WorkFeedback {
            phase: Some("hot-inference".into()), unit: Some("tokens".into()), backend: Some("cpu".into()),
            input_tokens: Some(tokens), latency_ms: Some(latency), throughput_per_second: Some(tokens as f64 * 1000.0 / latency),
            sequence_tokens: Some(sequence), queue_depth: Some(128), ..Default::default()
        };
        assert!(policy.report("embedding", sample(64, 10.0, 64), 1000));
        policy.report("embedding", sample(128, 15.0, 64), 7000);
        assert_eq!(policy.fraction("embedding", false), 2.0);
        assert_eq!(policy.latency_ms, Some(11.25));
        policy.report("embedding", sample(512, 5.0, 512), 13000);
        assert_eq!(policy.fraction("embedding", false), 2.0);
        policy.report("embedding", sample(64, 5.0, 64), 19000);
        assert_eq!(policy.fraction("embedding", false), 4.0);
        policy.report("embedding", sample(64, 2.0, 64), 25000);
        assert_eq!(policy.fraction("embedding", false), 4.0);
        policy.report("embedding", WorkFeedback { backend: Some("cpu".into()), allocation_failure: Some(true), ..Default::default() }, 25001);
        assert_eq!(policy.fraction("embedding", false), 0.5);
        assert!(!policy.report("embedding", sample(512, 1.0, 513), 30000));
    }
}
