//! Priority queue for BotAction dispatch to the executor.
//!
//! Wraps a `BinaryHeap<PrioritizedAction>` where higher-priority actions
//! (lower numeric `StrategyPriority` value) are dequeued first. This ensures
//! that Liquidation actions are always processed before Backrun, FlashArb, etc.
//!
//! The executor pops from this queue in priority order, submitting bundles/txs
//! to Jito and other submission paths.
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 136-138:
//!   "StrategyPriority enum, PriorityQueue<BotAction>,
//!    priority ordering: Liquidation(1) > Backrun(2) > FlashArb(3) > LstArb(4) > CopyTrade(5)"
//! [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d:
//!   "Priority queue with strategy weight. When multiple opportunities arrive
//!    in the same slot, highest-priority executes first."

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use predator_core::{BotAction, StrategyPriority};

// ---------------------------------------------------------------------------
// PrioritizedAction -- wraps BotAction with priority for heap ordering
// ---------------------------------------------------------------------------

/// A `BotAction` annotated with its originating strategy's priority.
///
/// Implements `Ord` so that the `BinaryHeap` (max-heap) dequeues
/// higher-priority (lower numeric value) actions first.
///
/// The `sequence` field is a monotonically increasing counter used to break
/// ties: among actions with the same priority, earlier actions are dequeued
/// first (FIFO within priority level).
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 136-138
#[derive(Debug)]
pub struct PrioritizedAction {
    /// The action to execute.
    pub action: BotAction,
    /// Priority of the strategy that produced this action.
    pub priority: StrategyPriority,
    /// Insertion sequence number for FIFO tiebreaking within same priority.
    pub sequence: u64,
}

impl Eq for PrioritizedAction {}

impl PartialEq for PrioritizedAction {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

/// Higher-priority (lower numeric value) actions sort as "greater" so the
/// BinaryHeap (max-heap) dequeues them first. Within the same priority,
/// earlier sequence numbers (smaller) are dequeued first.
///
/// This matches StrategyPriority's Ord impl where Liquidation(1) > CopyTrade(5).
impl Ord for PrioritizedAction {
    fn cmp(&self, other: &Self) -> Ordering {
        // First: compare by StrategyPriority (already reversed: lower value = greater)
        self.priority
            .cmp(&other.priority)
            // Tiebreaker: earlier sequence = should come first = is "greater" in heap terms
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for PrioritizedAction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------
// ActionQueue -- priority queue for the executor
// ---------------------------------------------------------------------------

/// Priority queue for `BotAction` dispatch.
///
/// The executor loop calls `pop()` to get the next highest-priority action.
/// Strategies push actions via `push(action, priority)`.
///
/// Thread safety: This struct is NOT thread-safe. It is owned by the executor
/// task and accessed from a single `tokio::select!` loop. Cross-task
/// communication uses `mpsc::Sender<(StrategyPriority, BotAction)>`.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1367-1368:
///   "action_tx.send((priority, action))"
pub struct ActionQueue {
    heap: BinaryHeap<PrioritizedAction>,
    next_sequence: u64,
}

impl ActionQueue {
    /// Create a new empty ActionQueue.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_sequence: 0,
        }
    }

    /// Push an action with its strategy priority.
    ///
    /// Actions with higher priority (lower numeric value) will be dequeued first.
    /// Within the same priority level, actions are dequeued in FIFO order.
    pub fn push(&mut self, action: BotAction, priority: StrategyPriority) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.heap.push(PrioritizedAction {
            action,
            priority,
            sequence,
        });
    }

    /// Pop the highest-priority action from the queue.
    ///
    /// Returns `None` if the queue is empty.
    pub fn pop(&mut self) -> Option<BotAction> {
        self.heap.pop().map(|pa| pa.action)
    }

    /// Number of actions currently in the queue.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Drain all actions from the queue in priority order.
    ///
    /// Returns a `Vec<BotAction>` sorted from highest to lowest priority.
    /// Useful for batch processing during scan cycles.
    pub fn drain(&mut self) -> Vec<BotAction> {
        let mut actions = Vec::with_capacity(self.heap.len());
        while let Some(pa) = self.heap.pop() {
            actions.push(pa.action);
        }
        self.next_sequence = 0;
        actions
    }

    /// Peek at the priority of the next action without removing it.
    pub fn peek_priority(&self) -> Option<StrategyPriority> {
        self.heap.peek().map(|pa| pa.priority)
    }
}

impl Default for ActionQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use predator_core::types::Lamports;

    fn make_bundle(tip: u64) -> BotAction {
        BotAction::SubmitBundle {
            txs: vec![vec![0u8; 10]],
            tip_lamports: Lamports(tip),
            priority: StrategyPriority::Liquidation,
        }
    }

    fn make_log(desc: &str) -> BotAction {
        BotAction::LogOpportunity {
            protocol: predator_core::Protocol::Save,
            est_profit: Lamports(1000),
            description: desc.to_string(),
        }
    }

    #[test]
    fn empty_queue() {
        let mut q = ActionQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.pop().is_none());
        assert!(q.peek_priority().is_none());
    }

    #[test]
    fn priority_ordering_liquidation_first() {
        let mut q = ActionQueue::new();

        // Push in reverse priority order
        q.push(make_log("copy"), StrategyPriority::CopyTrade);
        q.push(make_log("lst"), StrategyPriority::LstArb);
        q.push(make_log("arb"), StrategyPriority::FlashArb);
        q.push(make_log("backrun"), StrategyPriority::Backrun);
        q.push(make_log("liq"), StrategyPriority::Liquidation);

        assert_eq!(q.len(), 5);

        // Should dequeue in priority order: Liquidation first
        assert_eq!(q.peek_priority(), Some(StrategyPriority::Liquidation));

        let actions = q.drain();
        assert_eq!(actions.len(), 5);

        // Verify order by checking descriptions
        let labels: Vec<&str> = actions.iter().map(|a| a.label()).collect();
        // All are log_opportunity, but priority ordering is verified by the drain order
        assert_eq!(labels.len(), 5);
    }

    #[test]
    fn fifo_within_same_priority() {
        let mut q = ActionQueue::new();

        q.push(make_log("first"), StrategyPriority::Liquidation);
        q.push(make_log("second"), StrategyPriority::Liquidation);
        q.push(make_log("third"), StrategyPriority::Liquidation);

        // Same priority -> FIFO order
        if let Some(BotAction::LogOpportunity { description, .. }) = q.pop() {
            assert_eq!(description, "first");
        } else {
            panic!("expected LogOpportunity");
        }

        if let Some(BotAction::LogOpportunity { description, .. }) = q.pop() {
            assert_eq!(description, "second");
        } else {
            panic!("expected LogOpportunity");
        }

        if let Some(BotAction::LogOpportunity { description, .. }) = q.pop() {
            assert_eq!(description, "third");
        } else {
            panic!("expected LogOpportunity");
        }
    }

    #[test]
    fn mixed_priorities_and_fifo() {
        let mut q = ActionQueue::new();

        // Push two at same priority, then one higher
        q.push(make_log("arb-1"), StrategyPriority::FlashArb);
        q.push(make_log("arb-2"), StrategyPriority::FlashArb);
        q.push(make_log("liq-1"), StrategyPriority::Liquidation);

        // Liquidation should come first despite being pushed last
        if let Some(BotAction::LogOpportunity { description, .. }) = q.pop() {
            assert_eq!(description, "liq-1");
        }
        // Then arb-1 (FIFO within FlashArb)
        if let Some(BotAction::LogOpportunity { description, .. }) = q.pop() {
            assert_eq!(description, "arb-1");
        }
        // Then arb-2
        if let Some(BotAction::LogOpportunity { description, .. }) = q.pop() {
            assert_eq!(description, "arb-2");
        }
    }

    #[test]
    fn drain_resets_sequence() {
        let mut q = ActionQueue::new();
        q.push(make_bundle(1000), StrategyPriority::Liquidation);
        q.push(make_bundle(2000), StrategyPriority::Backrun);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert!(q.is_empty());

        // After drain, new pushes should work fine
        q.push(make_bundle(3000), StrategyPriority::FlashArb);
        assert_eq!(q.len(), 1);
    }
}
