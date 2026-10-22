//! Task-local progress and delivery priorities derived from actual work receipts.
//! 从真实工作回执派生任务进展与交付优先级；不执行工具、不授予权限、不拥有正式账本。

use serde::{Deserialize, Serialize};

const STAGNANT_ROUNDS_BEFORE_CORRECTION: u64 = 3;
const INITIAL_DISCOVERY_ROUNDS: u64 = 2;

/// The caller supplies source/range changes after canonical receipt validation.
/// 调用端在核对正式回执后提供来源与范围变化；调用理由、归档新 ID 或缓存命中不算新证据。
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoundProgress {
    pub completed_source_count: u64,
    pub evidence_changed: bool,
    pub artifact_updated: bool,
    pub new_read_range: bool,
    pub repeated_reads: u64,
    pub source_reads: u64,
    pub discovery_calls: u64,
    pub work_calls: u64,
    pub waiting_only: bool,
    /// An execution turn that only narrates unchanged work is stagnant, unlike an ordinary text answer.
    /// 执行阶段只叙述未变工作的轮次计为停滞；普通文字回答不传入此标记。
    pub execution_without_action: bool,
}

/// These counters may travel in the task checkpoint; stored document facts remain elsewhere.
/// 计数可随业务检查点保存；逐文档事实仍由现有工作记录保存，不复制另一份证据账本。
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkProgress {
    completed_source_count: u64,
    no_progress_rounds: u64,
    repeated_reads: u64,
    initial_discovery_rounds: u64,
    work_started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkProgressPlan {
    pub made_progress: bool,
    pub completed_source_count: u64,
    pub no_progress_rounds: u64,
    pub repeated_reads: u64,
    pub correction_due: bool,
    pub change_strategy: bool,
}

impl WorkProgress {
    pub fn stagnant_rounds(&self) -> u64 {
        self.no_progress_rounds
    }

    pub fn needs_strategy_change(&self) -> bool {
        self.no_progress_rounds >= STAGNANT_ROUNDS_BEFORE_CORRECTION
    }

    /// Stalled work can enter synthesis before its tool allowance is nearly spent.
    /// 工作停滞时提前转入综合建议；不改变授额，不把已读资料认定为任务完成。
    pub fn delivery_plan(&self, max_tool_calls: u64, used_tool_calls: u64) -> DeliveryBudgetPlan {
        let mut plan = delivery_budget_plan(max_tool_calls, used_tool_calls);
        if self.needs_strategy_change() && plan.phase == ToolBudgetPhase::Analyze {
            plan.phase = ToolBudgetPhase::Consolidate;
            plan.deprioritize_discovery_and_search = true;
        }
        plan
    }

    /// Count useful changes, not calls; genuine wait-only rounds leave stagnation unchanged.
    /// 统计有效变化而非调用数量；正式等待或状态轮询不增长、也不清除已有无进展轮次。
    pub fn observe_round(&mut self, round: &RoundProgress) -> WorkProgressPlan {
        let made_progress = round.completed_source_count > self.completed_source_count
            || round.evidence_changed
            || round.artifact_updated
            || round.new_read_range;
        self.completed_source_count = round.completed_source_count;
        self.repeated_reads = self.repeated_reads.saturating_add(round.repeated_reads);

        let has_calls = round.work_calls > 0
            || round.source_reads > 0
            || round.discovery_calls > 0
            || round.repeated_reads > 0
            || round.execution_without_action;
        if made_progress {
            self.work_started = true;
            self.no_progress_rounds = 0;
        } else if !round.waiting_only && has_calls {
            let initial_discovery = !self.work_started
                && round.source_reads == 0
                && round.repeated_reads == 0
                && round.discovery_calls > 0
                && round.work_calls <= round.discovery_calls;
            if initial_discovery && self.initial_discovery_rounds < INITIAL_DISCOVERY_ROUNDS {
                self.initial_discovery_rounds += 1;
            } else {
                self.work_started = true;
                self.no_progress_rounds = self.no_progress_rounds.saturating_add(1);
            }
        }

        let change_strategy = self.needs_strategy_change();
        // Remind once per three stalled work rounds rather than after every polling response.
        // 每三轮停滞工作提示一次，避免每次等待回执或每轮模型回复都追加纠偏文本。
        let correction_due = change_strategy
            && has_calls
            && !round.waiting_only
            && self.no_progress_rounds % STAGNANT_ROUNDS_BEFORE_CORRECTION == 0;
        WorkProgressPlan {
            made_progress,
            completed_source_count: self.completed_source_count,
            no_progress_rounds: self.no_progress_rounds,
            repeated_reads: self.repeated_reads,
            correction_due,
            change_strategy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolBudgetPhase {
    Analyze,
    Consolidate,
    Deliver,
}

/// Priority advice never changes the caller's granted limit or exempts cached calls.
/// 优先级建议不更改调用端真实授额，也不豁免缓存调用；耗尽工具额度仍可整理已有成果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryBudgetPlan {
    pub max_tool_calls: u64,
    pub used_tool_calls: u64,
    pub remaining_tool_calls: u64,
    pub delivery_reserve_calls: u64,
    pub analysis_allowance_calls: u64,
    pub phase: ToolBudgetPhase,
    pub deprioritize_discovery_and_search: bool,
    pub tools_exhausted: bool,
}

pub fn delivery_budget_plan(max_tool_calls: u64, used_tool_calls: u64) -> DeliveryBudgetPlan {
    let remaining_tool_calls = max_tool_calls.saturating_sub(used_tool_calls);
    // Reserve about one fifth for synthesis/verification, scaled down for small grants.
    // 常用额度约保留五分之一给综合和核验；小授额同比缩小，不锁死十二次调用。
    let proportional_reserve = max_tool_calls / 5 + u64::from(max_tool_calls % 5 != 0);
    let small_grant_cap = (max_tool_calls / 3).max(u64::from(max_tool_calls > 0));
    let delivery_reserve_calls = proportional_reserve.clamp(12, 48).min(small_grant_cap);
    let final_delivery_threshold = (delivery_reserve_calls / 4).clamp(1, 4);
    let phase = if remaining_tool_calls <= final_delivery_threshold {
        ToolBudgetPhase::Deliver
    } else if remaining_tool_calls <= delivery_reserve_calls {
        ToolBudgetPhase::Consolidate
    } else {
        ToolBudgetPhase::Analyze
    };

    DeliveryBudgetPlan {
        max_tool_calls,
        used_tool_calls,
        remaining_tool_calls,
        delivery_reserve_calls,
        analysis_allowance_calls: remaining_tool_calls.saturating_sub(delivery_reserve_calls),
        phase,
        deprioritize_discovery_and_search: phase != ToolBudgetPhase::Analyze,
        tools_exhausted: remaining_tool_calls == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_prose_without_action_changes_strategy_without_ending_or_spending_tools() {
        let mut progress = WorkProgress::default();
        for round in 1..=3 {
            let plan = progress.observe_round(&RoundProgress { execution_without_action:true,..Default::default() });
            assert_eq!(plan.no_progress_rounds,round);
            assert_eq!(plan.correction_due,round==3);
        }
        assert!(progress.needs_strategy_change());
        assert_eq!(progress.delivery_plan(256,14).remaining_tool_calls,242);
        let useful = progress.observe_round(&RoundProgress { new_read_range:true,source_reads:1,..Default::default() });
        assert!(useful.made_progress); assert_eq!(useful.no_progress_rounds,0);
    }

    #[test]
    fn repeated_reads_correct_strategy_without_blocking_new_evidence() {
        let mut progress = WorkProgress::default();
        let first = progress.observe_round(&RoundProgress {
            completed_source_count: 1,
            source_reads: 1,
            new_read_range: true,
            ..Default::default()
        });
        assert!(first.made_progress);
        for index in 1..=3 {
            let plan = progress.observe_round(&RoundProgress {
                completed_source_count: 1,
                source_reads: 1,
                repeated_reads: 1,
                ..Default::default()
            });
            assert_eq!(plan.no_progress_rounds, index);
            assert_eq!(plan.correction_due, index == 3);
            assert_eq!(progress.stagnant_rounds(), index);
            assert_eq!(progress.needs_strategy_change(), index == 3);
            let delivery = progress.delivery_plan(80, 29);
            assert_eq!(delivery.max_tool_calls, 80);
            assert_eq!(delivery.remaining_tool_calls, 51);
            assert_eq!(delivery.deprioritize_discovery_and_search, index == 3);
            assert_eq!(
                delivery.phase,
                if index == 3 {
                    ToolBudgetPhase::Consolidate
                } else {
                    ToolBudgetPhase::Analyze
                }
            );
        }
        let resumed = progress.observe_round(&RoundProgress {
            completed_source_count: 2,
            artifact_updated: true,
            ..Default::default()
        });
        assert!(resumed.made_progress);
        assert!(!resumed.change_strategy);
        assert_eq!(resumed.repeated_reads, 3);
        assert_eq!(progress.stagnant_rounds(), 0);
        assert!(!progress.needs_strategy_change());
        assert_eq!(
            progress.delivery_plan(80, 29).phase,
            ToolBudgetPhase::Analyze
        );
    }

    #[test]
    fn setup_and_real_polling_do_not_fake_progress_or_trigger_early_correction() {
        let mut progress = WorkProgress::default();
        for _ in 0..2 {
            let plan = progress.observe_round(&RoundProgress {
                discovery_calls: 1,
                work_calls: 1,
                ..Default::default()
            });
            assert_eq!(plan.no_progress_rounds, 0);
            assert!(!plan.made_progress);
        }
        let plan = progress.observe_round(&RoundProgress {
            discovery_calls: 1,
            work_calls: 1,
            ..Default::default()
        });
        assert_eq!(plan.no_progress_rounds, 1);
        let before_wait = serde_json::to_value(&progress).unwrap();
        for _ in 0..4 {
            let waiting = progress.observe_round(&RoundProgress {
                work_calls: 1,
                waiting_only: true,
                ..Default::default()
            });
            assert!(!waiting.correction_due);
            assert_eq!(waiting.no_progress_rounds, 1);
        }
        assert_eq!(serde_json::to_value(&progress).unwrap(), before_wait);
    }

    #[test]
    fn granted_budget_keeps_delivery_room_and_never_increases_the_limit() {
        let ordinary = delivery_budget_plan(80, 64);
        assert_eq!(ordinary.delivery_reserve_calls, 16);
        assert_eq!(ordinary.remaining_tool_calls, 16);
        assert_eq!(ordinary.phase, ToolBudgetPhase::Consolidate);
        assert_eq!(delivery_budget_plan(256, 0).delivery_reserve_calls, 48);
        assert_eq!(delivery_budget_plan(8, 0).delivery_reserve_calls, 2);
        assert_eq!(delivery_budget_plan(80, 76).phase, ToolBudgetPhase::Deliver);
        let exhausted = delivery_budget_plan(80, 81);
        assert!(exhausted.tools_exhausted);
        assert_eq!(exhausted.remaining_tool_calls, 0);
        assert_eq!(exhausted.max_tool_calls, 80);
        assert_eq!(exhausted.used_tool_calls, 81);
        assert_eq!(delivery_budget_plan(u64::MAX, 0).delivery_reserve_calls, 48);
    }
}
