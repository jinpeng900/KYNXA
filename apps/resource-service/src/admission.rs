use crate::budget::{AllocationRequest, ResourceBudget};
use crate::sampling::HardwareSnapshot;
use serde_json::{Value, json};

const MAX_WAITERS: usize = 32;
struct Waiting {
    id: u64,
    request: AllocationRequest,
    queued_at: u64,
    deadline: u64,
}
pub struct AdmissionQueue {
    waiting: Vec<Waiting>,
    foreground_streak: u32,
}
impl AdmissionQueue {
    pub fn new() -> Self {
        Self {
            waiting: Vec::new(),
            foreground_streak: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.waiting.len()
    }
    pub fn enqueue(&mut self, id: u64, request: AllocationRequest, now_ms: u64) -> Option<Value> {
        if self.waiting.len() >= MAX_WAITERS {
            return Some(json!({"status":"denied","reason":"RESOURCE_QUEUE_FULL"}));
        }
        let deadline = now_ms + request.wait_ms.min(60_000);
        self.waiting.push(Waiting {
            id,
            request,
            queued_at: now_ms,
            deadline,
        });
        None
    }
    pub fn cancel(&mut self, id: u64) -> Option<(u64, Value)> {
        let index = self.waiting.iter().position(|waiter| waiter.id == id)?;
        self.waiting.remove(index);
        Some((id, json!({"status":"denied","reason":"RESOURCE_CANCELLED"})))
    }
    pub fn drain(
        &mut self,
        budget: &mut ResourceBudget,
        hardware: &HardwareSnapshot,
        now_ms: u64,
    ) -> Vec<(u64, Value)> {
        let mut results = Vec::new();
        let mut index = 0;
        while index < self.waiting.len() {
            if self.waiting[index].deadline <= now_ms {
                let expired = self.waiting.remove(index);
                results.push((
                    expired.id,
                    json!({"status":"denied","reason":"RESOURCE_WAIT_TIMEOUT"}),
                ));
            } else {
                index += 1;
            }
        }
        // Every fourth admission gives background work a chance; aging also overtakes foreground.
        // 每四次准入给后台一次机会；等待超过两秒的后台也可优先，避免持续前台任务饿死索引。
        let mut index = 0;
        while index < self.waiting.len() {
            self.waiting.sort_by_key(|waiter| {
                let is_background = waiter.request.kind == "background";
                let aged = is_background
                    && (now_ms.saturating_sub(waiter.queued_at) >= 2000
                        || self.foreground_streak >= 3);
                (
                    if aged {
                        0
                    } else if !is_background {
                        1
                    } else {
                        2
                    },
                    waiter.queued_at,
                )
            });
            let outcome = budget.acquire(self.waiting[index].request.clone(), hardware, now_ms);
            if outcome["status"] == "granted" || outcome["reason"] != "RESOURCE_PRESSURE" {
                let waiter = self.waiting.remove(index);
                if outcome["status"] == "granted" {
                    self.foreground_streak = if waiter.request.kind == "foreground" {
                        self.foreground_streak + 1
                    } else {
                        0
                    };
                }
                results.push((waiter.id, outcome));
                index = 0;
            } else {
                index += 1;
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuSnapshot;
    use crate::sampling::{CpuSnapshot, MemorySnapshot};
    fn hardware() -> HardwareSnapshot {
        HardwareSnapshot {
            cpu: CpuSnapshot {
                logical_cores: 4,
                usage_percent: Some(1.0),
            },
            memory: MemorySnapshot {
                total_bytes: 8 << 30,
                available_bytes: 4 << 30,
            },
            gpu: GpuSnapshot::unknown("fixture"),
        }
    }
    fn request(kind: &str) -> AllocationRequest {
        AllocationRequest {
            task_id: kind.into(),
            workspace_id: "fixture".into(),
            kind: kind.into(),
            workload: "compute".into(),
            cpu_threads: 2,
            memory_bytes: 0,
            gpu_memory_bytes: 0,
            ttl_ms: 10000,
            wait_ms: 5000,
        }
    }
    #[test]
    fn foreground_preference_background_aging_and_cancel_have_real_admission_effects() {
        let mut budget = ResourceBudget::new();
        let hardware = hardware();
        let mut queue = AdmissionQueue::new();
        let owner = budget.acquire(request("foreground"), &hardware, 0);
        queue.enqueue(1, request("background"), 1);
        queue.enqueue(2, request("foreground"), 2);
        assert!(queue.drain(&mut budget, &hardware, 3).is_empty());
        budget.release(owner["leaseId"].as_str().unwrap());
        let first = queue.drain(&mut budget, &hardware, 4);
        assert_eq!(first[0].0, 2);
        budget.release(first[0].1["leaseId"].as_str().unwrap());
        queue.enqueue(3, request("foreground"), 3000);
        let aged = queue.drain(&mut budget, &hardware, 3001);
        assert_eq!(aged[0].0, 1);
        assert_eq!(queue.cancel(3).unwrap().1["reason"], "RESOURCE_CANCELLED");
        budget.release(aged[0].1["leaseId"].as_str().unwrap());
        assert!(queue.drain(&mut budget, &hardware, 4000).is_empty());
    }
    #[test]
    fn queue_has_bounded_length_and_deadline() {
        let mut budget = ResourceBudget::new();
        let hardware = hardware();
        let mut queue = AdmissionQueue::new();
        for id in 1..=32 {
            assert!(queue.enqueue(id, request("background"), 0).is_none());
        }
        assert_eq!(
            queue.enqueue(33, request("background"), 0).unwrap()["reason"],
            "RESOURCE_QUEUE_FULL"
        );
        let results = queue.drain(&mut budget, &hardware, 6000);
        assert_eq!(results.len(), 32);
        assert!(
            results
                .iter()
                .all(|(_, result)| result["reason"] == "RESOURCE_WAIT_TIMEOUT")
        );
    }

    #[test]
    fn fourth_admission_gives_background_a_chance_in_the_same_drain() {
        let mut budget = ResourceBudget::new();
        let mut machine = hardware();
        machine.cpu.logical_cores = 8;
        let mut queue = AdmissionQueue::new();
        for id in 1..=4 {
            let mut work = request("foreground");
            work.cpu_threads = 1;
            queue.enqueue(id, work, 0);
        }
        let mut background = request("background");
        background.cpu_threads = 1;
        queue.enqueue(5, background, 0);
        let completed = queue.drain(&mut budget, &machine, 1);
        assert_eq!(completed[3].0, 5);
    }
}
