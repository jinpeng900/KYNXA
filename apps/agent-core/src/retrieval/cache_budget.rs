//! Adaptive cache targets are replaceable optimization policy, never task state or hardware authority.
//! 自适应缓存目标仅是可重建优化策略，不承担任务状态，也不替代硬件资源服务授额。

use serde_json::{Value, json};

pub(crate) const MIB: usize = 1024 * 1024;
const ADJUST_INTERVAL_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheKind {
    QueryVectors,
    RerankScores,
    Candidates,
}
impl CacheKind {
    pub(crate) const ALL: [Self; 3] = [Self::QueryVectors, Self::RerankScores, Self::Candidates];
    pub(crate) fn index(self) -> usize {
        match self {
            Self::QueryVectors => 0,
            Self::RerankScores => 1,
            Self::Candidates => 2,
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::QueryVectors => "query-vectors",
            Self::RerankScores => "rerank-scores",
            Self::Candidates => "retrieval-candidates",
        }
    }
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Feedback {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub occupied_bytes: usize,
    pub computed_micros: u64,
    pub estimated_saved_micros: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HardwareHeadroom {
    pub available_bytes: usize,
    pub capacity_bytes: usize,
    pub physical_total_bytes: Option<usize>,
    pub physical_available_bytes: Option<usize>,
    pub approved_cache_bytes: usize,
    pub queued: bool,
    pub foreground_latency_micros: Option<u64>,
}
impl HardwareHeadroom {
    fn limit(self) -> usize {
        let reusable = self
            .available_bytes
            .saturating_add(self.approved_cache_bytes);
        let mut limit = (reusable / 4).min(self.capacity_bytes / 8);
        if let Some(total) = self.physical_total_bytes {
            limit = limit.min(total / 32);
        }
        if let Some(free) = self.physical_available_bytes {
            limit = limit.min(free.saturating_add(self.approved_cache_bytes) / 4);
        }
        limit
    }
    fn pressured(self) -> bool {
        self.queued
            || self.available_bytes < self.capacity_bytes / 100
            || self
                .physical_total_bytes
                .zip(self.physical_available_bytes)
                .is_some_and(|(total, free)| free < total / 20)
    }
}

pub(crate) struct BudgetController {
    enabled: [bool; 3],
    seeds: [usize; 3],
    caps: [Option<usize>; 3],
    feedback: [Feedback; 3],
    previous: [Feedback; 3],
    targets: [usize; 3],
    total: usize,
    hardware_limit: usize,
    total_cap: Option<usize>,
    last_adjust: Option<u64>,
    weights: [f64; 3],
    idle_windows: u64,
    foreground_baseline: f64,
    foreground_recent: f64,
    last_latency_sample: Option<u64>,
    dirty: bool,
    reason: &'static str,
}
impl Default for BudgetController {
    fn default() -> Self {
        Self {
            enabled: [false; 3],
            seeds: [32 * MIB, 16 * MIB, 32 * MIB],
            caps: [None; 3],
            feedback: [Feedback::default(); 3],
            previous: [Feedback::default(); 3],
            targets: [0; 3],
            total: 0,
            hardware_limit: 0,
            total_cap: None,
            last_adjust: None,
            weights: [32.0, 16.0, 32.0],
            idle_windows: 0,
            foreground_baseline: 0.0,
            foreground_recent: 0.0,
            last_latency_sample: None,
            dirty: false,
            reason: "not-sampled",
        }
    }
}
impl BudgetController {
    pub(crate) fn register(&mut self, kind: CacheKind, seed: usize) {
        let i = kind.index();
        if !self.enabled[i] || self.seeds[i] != seed {
            self.enabled[i] = true;
            self.seeds[i] = seed;
            self.dirty = true;
        }
    }
    pub(crate) fn manual_limit(&mut self, kind: CacheKind, cap: Option<usize>) -> bool {
        let i = kind.index();
        if self.caps[i] != cap {
            self.caps[i] = cap;
            self.dirty = true;
            true
        } else {
            false
        }
    }
    pub(crate) fn manual_cap(&self, kind: CacheKind) -> Option<usize> {
        self.caps[kind.index()]
    }
    pub(crate) fn total_limit(&mut self, cap: Option<usize>) {
        if self.total_cap != cap {
            self.total_cap = cap;
            self.dirty = true;
        }
    }
    pub(crate) fn observe(&mut self, kind: CacheKind, feedback: Feedback) {
        self.feedback[kind.index()] = feedback;
    }
    pub(crate) fn latency(&mut self, micros: u64) {
        if micros == 0 {
            return;
        }
        if self.foreground_baseline == 0.0 {
            self.foreground_baseline = micros as f64;
            self.foreground_recent = micros as f64;
        } else {
            self.foreground_baseline = 0.98 * self.foreground_baseline + 0.02 * micros as f64;
            self.foreground_recent = 0.75 * self.foreground_recent + 0.25 * micros as f64;
        }
    }
    pub(crate) fn target(&self, kind: CacheKind) -> usize {
        self.targets[kind.index()]
    }
    pub(crate) fn reclaimed(&mut self) {
        self.targets = [0; 3];
        self.total = 0;
        self.dirty = true;
        self.reason = "cache-memory-reclaimed";
    }
    pub(crate) fn update(&mut self, now_ms: u64, hardware: HardwareHeadroom) {
        self.hardware_limit = hardware.limit().min(self.total_cap.unwrap_or(usize::MAX));
        if let Some(latency) = hardware
            .foreground_latency_micros
            .filter(|latency| Some(*latency) != self.last_latency_sample)
        {
            self.latency(latency);
            self.last_latency_sample = Some(latency);
        }
        if hardware.pressured() || self.hardware_limit == 0 {
            self.targets = [0; 3];
            self.total = 0;
            self.reason = "hardware-pressure";
            self.last_adjust = Some(now_ms);
            return;
        }
        let must_shrink = self
            .targets
            .iter()
            .fold(0usize, |total, bytes| total.saturating_add(*bytes))
            > self.hardware_limit
            || (0..3).any(|i| self.targets[i] > self.caps[i].unwrap_or(usize::MAX));
        if !self.dirty
            && !must_shrink
            && self
                .last_adjust
                .is_some_and(|last| now_ms.saturating_sub(last) < ADJUST_INTERVAL_MS)
        {
            return;
        }
        let mut requests = 0u64;
        let mut useful_evictions = false;
        let mut weights = [0.0; 3];
        for i in 0..3 {
            if !self.enabled[i] || self.caps[i] == Some(0) {
                continue;
            }
            let current = self.feedback[i];
            let old = self.previous[i];
            let hits = current.hits.saturating_sub(old.hits);
            let misses = current.misses.saturating_sub(old.misses);
            let evictions = current.evictions.saturating_sub(old.evictions);
            let saved = current
                .estimated_saved_micros
                .saturating_sub(old.estimated_saved_micros);
            let computed = current.computed_micros.saturating_sub(old.computed_micros);
            requests = requests.saturating_add(hits).saturating_add(misses);
            let hit_rate = hits as f64 / (hits.saturating_add(misses).max(1) as f64);
            let density = ((saved as f64 + computed as f64 * hit_rate) / 1000.0)
                / (current.occupied_bytes.max(MIB) as f64 / MIB as f64);
            let measured =
                (self.seeds[i] as f64 / MIB as f64) * (1.0 + 3.0 * hit_rate + (1.0 + density).ln());
            if self.last_adjust.is_some() {
                self.weights[i] = self.weights[i] * 0.5 + measured * 0.5;
            } else {
                self.weights[i] = self.seeds[i] as f64 / MIB as f64;
            }
            weights[i] = self.weights[i];
            useful_evictions |= evictions > 0
                && (hits > 0 || saved > 0)
                && current.occupied_bytes >= self.targets[i].saturating_mul(3) / 4;
        }
        let seeds = (0..3)
            .filter(|i| self.enabled[*i] && self.caps[*i] != Some(0))
            .map(|i| self.seeds[i].min(self.caps[i].unwrap_or(usize::MAX)))
            .fold(0usize, usize::saturating_add);
        if requests == 0 {
            self.idle_windows = self.idle_windows.saturating_add(1);
        } else {
            self.idle_windows = 0;
        }
        let latency_pressure = self.foreground_baseline > 0.0
            && self.foreground_recent > self.foreground_baseline * 1.75;
        let next = if self.last_adjust.is_none() || self.total == 0 || self.dirty {
            self.reason = "initial-hardware-share";
            seeds.max(self.total)
        } else if latency_pressure {
            self.reason = "local-foreground-latency";
            self.total.saturating_mul(3) / 4
        } else if self.idle_windows >= 3 {
            self.reason = "idle-cache-shrink";
            self.total.saturating_mul(3) / 4
        } else if requests > 0 && self.total < seeds / 2 {
            self.reason = "active-cache-rewarm";
            seeds
        } else if useful_evictions {
            self.reason = "measured-reuse-growth";
            self.total.saturating_add((self.total / 4).max(1))
        } else {
            self.reason = "measured-benefit-rebalance";
            self.total
        };
        self.total = next.min(self.hardware_limit);
        let mut targets = allocate(self.total, &weights, &self.caps);
        // Small normal variations keep their grant; hardware/cap reductions always win immediately.
        // 正常小幅波动保留已有授额；硬件或手动上限降低时立即优先收缩。
        if !must_shrink && !self.dirty {
            for i in 0..3 {
                if targets[i].abs_diff(self.targets[i]) < self.targets[i] / 8 {
                    targets[i] = self.targets[i];
                }
            }
            let sum = targets
                .iter()
                .fold(0usize, |total, bytes| total.saturating_add(*bytes));
            if sum > self.total {
                for target in &mut targets {
                    *target = ((*target as u128 * self.total as u128) / sum as u128) as usize;
                }
            }
        }
        self.targets = targets;
        self.total = self
            .targets
            .iter()
            .fold(0usize, |total, bytes| total.saturating_add(*bytes));
        self.previous = self.feedback;
        self.last_adjust = Some(now_ms);
        self.dirty = false;
    }
    pub(crate) fn stats(&self) -> Value {
        json!({"automatic":true,"totalTargetBytes":self.total,"hardwareLimitBytes":self.hardware_limit,
            "adjustmentIntervalMs":ADJUST_INTERVAL_MS,"hysteresisFraction":0.125,"reason":self.reason,
            "pendingRecalculation":self.dirty,
            "targets":CacheKind::ALL.map(|kind|json!({"name":kind.name(),"bytes":self.target(kind),"manualLimitBytes":self.caps[kind.index()]})),
            "localForegroundBaselineMicros":self.foreground_baseline,"localForegroundRecentMicros":self.foreground_recent})
    }
}

fn allocate(total: usize, weights: &[f64; 3], caps: &[Option<usize>; 3]) -> [usize; 3] {
    let mut result = [0usize; 3];
    let mut remaining = total;
    for _ in 0..3 {
        let eligible = (0..3)
            .filter(|i| weights[*i] > 0.0 && result[*i] < caps[*i].unwrap_or(usize::MAX))
            .collect::<Vec<_>>();
        let weight = eligible.iter().map(|i| weights[*i]).sum::<f64>();
        if weight <= 0.0 || remaining == 0 {
            break;
        }
        let budget = remaining;
        let mut spent = 0;
        for i in eligible {
            let share =
                ((budget as f64 * weights[i] / weight) as usize).min(budget.saturating_sub(spent));
            let grant = share.min(caps[i].unwrap_or(usize::MAX).saturating_sub(result[i]));
            result[i] = result[i].saturating_add(grant);
            spent = spent.saturating_add(grant);
        }
        remaining = remaining.saturating_sub(spent);
        if spent == 0 {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hardware() -> HardwareHeadroom {
        HardwareHeadroom {
            available_bytes: 8 * 1024 * MIB,
            capacity_bytes: 8 * 1024 * MIB,
            physical_total_bytes: Some(32 * 1024 * MIB),
            physical_available_bytes: Some(16 * 1024 * MIB),
            ..Default::default()
        }
    }
    fn controller() -> BudgetController {
        let mut state = BudgetController::default();
        for kind in CacheKind::ALL {
            state.register(
                kind,
                if kind == CacheKind::RerankScores {
                    16 * MIB
                } else {
                    32 * MIB
                },
            );
        }
        state
    }
    #[test]
    fn automatic_growth_crosses_old_product_caps_only_with_measured_benefit() {
        let mut state = controller();
        state.update(0, hardware());
        for round in 1..18u64 {
            for kind in CacheKind::ALL {
                let previous = state.feedback[kind.index()];
                state.observe(
                    kind,
                    Feedback {
                        hits: previous.hits + 100,
                        misses: previous.misses + 20,
                        evictions: previous.evictions + 10,
                        occupied_bytes: state.target(kind),
                        computed_micros: previous.computed_micros + 100_000,
                        estimated_saved_micros: previous.estimated_saved_micros + 10_000_000,
                    },
                );
            }
            state.update(round * ADJUST_INTERVAL_MS, hardware());
        }
        assert!(state.target(CacheKind::QueryVectors) > 128 * MIB);
        assert!(state.targets.iter().sum::<usize>() <= state.hardware_limit);
    }
    #[test]
    fn hardware_pressure_and_manual_disable_yield_without_a_task_error() {
        let mut state = controller();
        state.update(0, hardware());
        state.manual_limit(CacheKind::QueryVectors, Some(0));
        state.update(1, hardware());
        assert_eq!(state.target(CacheKind::QueryVectors), 0);
        state.update(
            2,
            HardwareHeadroom {
                queued: true,
                ..hardware()
            },
        );
        assert_eq!(state.total, 0);
        state.update(31_000, hardware());
        assert!(state.total > 0);
    }
    #[test]
    fn bounds_hysteresis_and_adjustment_interval_prevent_budget_churn() {
        let mut state = controller();
        state.update(0, hardware());
        let targets = state.targets;
        state.update(
            1,
            HardwareHeadroom {
                available_bytes: 7 * 1024 * MIB,
                ..hardware()
            },
        );
        assert_eq!(state.targets, targets);
        state.manual_limit(CacheKind::RerankScores, Some(2 * MIB));
        state.update(2, hardware());
        assert!(state.target(CacheKind::RerankScores) <= 2 * MIB);
        state.update(
            3,
            HardwareHeadroom {
                available_bytes: 16 * MIB,
                capacity_bytes: 2 * 1024 * MIB,
                physical_available_bytes: None,
                ..hardware()
            },
        );
        assert_eq!(state.total, 0);
    }
    #[test]
    fn real_savings_rebalance_towards_useful_cache_and_idle_shrinks() {
        let mut state = controller();
        state.update(0, hardware());
        state.observe(
            CacheKind::Candidates,
            Feedback {
                hits: 100,
                misses: 1,
                occupied_bytes: MIB,
                estimated_saved_micros: 100_000_000,
                ..Default::default()
            },
        );
        state.update(30_000, hardware());
        assert!(state.target(CacheKind::Candidates) > state.target(CacheKind::QueryVectors));
        let before = state.total;
        for now in [60_000, 90_000, 120_000] {
            state.update(now, hardware());
        }
        assert!(state.total < before);
    }
    #[test]
    fn local_latency_regression_reduces_optional_budget_without_using_cloud_ttft() {
        let mut state = controller();
        state.update(0, hardware());
        state.latency(1000);
        for _ in 0..8 {
            state.latency(20_000);
        }
        let before = state.total;
        state.update(30_000, hardware());
        assert!(state.total < before);
        assert_eq!(state.reason, "local-foreground-latency");
    }
}
