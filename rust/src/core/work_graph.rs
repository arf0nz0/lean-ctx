//! Bounded Work Graph for multi-agent orchestration (P11 / DIM 4).
//!
//! Manages parent/child agent delegation with:
//! - Budget inheritance (child cannot exceed parent)
//! - Fan-out limits (max concurrent children)
//! - Stop conditions (stale, over-budget, redundant)
//! - Provenance tracking for attribution

use std::collections::BTreeMap;

use crate::core::a2a::budget_cascade::{
    BudgetAllocation, CascadeError, cascade_budget, validate_cascade,
};
use serde::{Deserialize, Serialize};

pub(crate) const WORK_GRAPH_SCHEMA_VERSION: u16 = 1;
const MAX_GRAPH_NODES: usize = 256;
const MAX_FAN_OUT: usize = 16;
const MAX_DEPTH: u16 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NodeStatus {
    Pending,
    Active,
    Completed,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StopReason {
    BudgetExhausted,
    Stale,
    Redundant,
    ParentStopped,
    ManualStop,
    DepthExceeded,
    FanOutExceeded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkNodeBudget {
    pub tokens_allocated: u64,
    pub tokens_consumed: u64,
    pub cost_micros_allocated: u64,
    pub cost_micros_consumed: u64,
}

impl WorkNodeBudget {
    pub(crate) fn tokens_remaining(&self) -> u64 {
        self.tokens_allocated.saturating_sub(self.tokens_consumed)
    }

    pub(crate) fn cost_remaining(&self) -> u64 {
        self.cost_micros_allocated
            .saturating_sub(self.cost_micros_consumed)
    }

    pub(crate) fn is_exhausted(&self) -> bool {
        self.tokens_remaining() == 0 || self.cost_remaining() == 0
    }
}

/// Tracks the total budget across an entire delegation chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChainBudget {
    pub chain_id: String,
    pub root_budget_tokens: u64,
    pub total_consumed_tokens: u64,
    pub total_allocated_tokens: u64,
    pub depth: u16,
}

impl ChainBudget {
    pub(crate) fn remaining(&self) -> u64 {
        self.root_budget_tokens
            .saturating_sub(self.total_consumed_tokens)
    }

    pub(crate) fn utilization_pct(&self) -> f64 {
        if self.root_budget_tokens == 0 {
            return 0.0;
        }

        (self.total_consumed_tokens as f64 / self.root_budget_tokens as f64) * 100.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkNode {
    pub node_id: String,
    pub agent_id: String,
    pub parent_node_id: Option<String>,
    pub capsule_ref: String,
    pub status: NodeStatus,
    pub budget: WorkNodeBudget,
    pub depth: u16,
    pub stop_reason: Option<StopReason>,
    pub outcome_ref: Option<String>,
}

/// Bounded, acyclic work graph with enforced fan-out and budget constraints.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BoundedWorkGraph {
    nodes: BTreeMap<String, WorkNode>,
    children: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    chain_budgets: BTreeMap<String, ChainBudget>,
    #[serde(default)]
    pending_child_budgets: BTreeMap<String, WorkNodeBudget>,
    max_fan_out: usize,
    max_depth: u16,
}

impl Default for BoundedWorkGraph {
    fn default() -> Self {
        Self::new(MAX_FAN_OUT, MAX_DEPTH)
    }
}

impl BoundedWorkGraph {
    #[must_use]
    pub(crate) fn new(max_fan_out: usize, max_depth: u16) -> Self {
        Self {
            nodes: BTreeMap::new(),
            children: BTreeMap::new(),
            chain_budgets: BTreeMap::new(),
            pending_child_budgets: BTreeMap::new(),
            max_fan_out: max_fan_out.clamp(1, MAX_FAN_OUT),
            max_depth: max_depth.clamp(1, MAX_DEPTH),
        }
    }

    /// Add a root node (no parent).
    pub(crate) fn add_root(
        &mut self,
        node_id: String,
        agent_id: String,
        capsule_ref: String,
        budget: WorkNodeBudget,
    ) -> Result<&WorkNode, WorkGraphError> {
        if self.nodes.len() >= MAX_GRAPH_NODES {
            return Err(WorkGraphError::CapacityExceeded);
        }
        if self.nodes.contains_key(&node_id) {
            return Err(WorkGraphError::DuplicateNode(node_id));
        }
        let node = WorkNode {
            node_id: node_id.clone(),
            agent_id,
            parent_node_id: None,
            capsule_ref,
            status: NodeStatus::Active,
            budget,
            depth: 0,
            stop_reason: None,
            outcome_ref: None,
        };
        self.nodes.insert(node_id.clone(), node);
        self.chain_budgets.insert(
            node_id.clone(),
            ChainBudget {
                chain_id: node_id.clone(),
                root_budget_tokens: self.nodes[&node_id].budget.tokens_allocated,
                total_consumed_tokens: 0,
                total_allocated_tokens: self.nodes[&node_id].budget.tokens_allocated,
                depth: 0,
            },
        );
        Ok(self.nodes.get(&node_id).expect("root node inserted above"))
    }

    /// Delegate work to a child node. Validates budget inheritance and fan-out.
    pub(crate) fn delegate(
        &mut self,
        parent_node_id: &str,
        child_node_id: String,
        child_agent_id: String,
        capsule_ref: String,
        child_budget: WorkNodeBudget,
    ) -> Result<&WorkNode, WorkGraphError> {
        if self.nodes.len() >= MAX_GRAPH_NODES {
            return Err(WorkGraphError::CapacityExceeded);
        }
        if self.nodes.contains_key(&child_node_id) {
            return Err(WorkGraphError::DuplicateNode(child_node_id));
        }
        let parent = self
            .nodes
            .get(parent_node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(parent_node_id.to_string()))?;
        if parent.status != NodeStatus::Active {
            return Err(WorkGraphError::ParentNotActive(parent_node_id.to_string()));
        }
        let new_depth = parent.depth + 1;
        if new_depth > self.max_depth {
            return Err(WorkGraphError::DepthExceeded(self.max_depth));
        }
        if child_budget.tokens_allocated > parent.budget.tokens_remaining() {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: child_budget.tokens_allocated,
                parent_remaining: parent.budget.tokens_remaining(),
            });
        }
        if child_budget.cost_micros_allocated > parent.budget.cost_remaining() {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: child_budget.cost_micros_allocated,
                parent_remaining: parent.budget.cost_remaining(),
            });
        }
        let current_children = self.children.get(parent_node_id).map_or(0, Vec::len);
        if current_children >= self.max_fan_out {
            return Err(WorkGraphError::FanOutExceeded(self.max_fan_out));
        }
        let child_tokens_allocated = child_budget.tokens_allocated;
        let node = WorkNode {
            node_id: child_node_id.clone(),
            agent_id: child_agent_id,
            parent_node_id: Some(parent_node_id.to_string()),
            capsule_ref,
            status: NodeStatus::Active,
            budget: child_budget,
            depth: new_depth,
            stop_reason: None,
            outcome_ref: None,
        };
        self.nodes.insert(child_node_id.clone(), node);
        self.children
            .entry(parent_node_id.to_string())
            .or_default()
            .push(child_node_id.clone());
        let chain_id = self
            .chain_id_for_node(&child_node_id)
            .expect("delegated child always has a root node");
        let pending_tokens = self
            .pending_child_budgets
            .remove(&child_node_id)
            .map_or(0, |budget| budget.tokens_allocated);
        self.record_chain_allocation(&chain_id, pending_tokens, child_tokens_allocated, new_depth);
        Ok(self
            .nodes
            .get(&child_node_id)
            .expect("child node inserted above"))
    }

    /// Allocates budget for a child node using cascade rules.
    ///
    /// The returned budget is reserved for `child_id` until it is passed to
    /// [`Self::delegate`], so chain allocation is not counted twice.
    pub(crate) fn allocate_child_budget(
        &mut self,
        parent_id: &str,
        child_id: &str,
        fraction: f64,
    ) -> Result<WorkNodeBudget, WorkGraphError> {
        let (parent_budget_tokens, parent_used_tokens, parent_cost_remaining, parent_depth) = {
            let parent = self
                .nodes
                .get(parent_id)
                .ok_or_else(|| WorkGraphError::NodeNotFound(parent_id.to_string()))?;
            if parent.status != NodeStatus::Active {
                return Err(WorkGraphError::ParentNotActive(parent_id.to_string()));
            }
            (
                parent.budget.tokens_allocated,
                parent.budget.tokens_consumed,
                parent.budget.cost_remaining(),
                parent.depth,
            )
        };

        let parent_remaining = parent_budget_tokens.saturating_sub(parent_used_tokens);
        if parent_remaining == 0 {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: 1,
                parent_remaining,
            });
        }

        let allocation = BudgetAllocation {
            parent_budget_tokens,
            parent_used_tokens,
            child_fraction: fraction,
            minimum_budget: 0,
            maximum_budget: parent_remaining,
        };
        let mut cascaded = cascade_budget(&allocation);
        cascaded.depth = u32::from(parent_depth) + 1;
        cascaded.lineage = self.node_lineage(parent_id);
        cascaded.lineage.push(child_id.to_string());
        validate_cascade(&cascaded)?;
        if cascaded.allocated_tokens > parent_remaining {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: cascaded.allocated_tokens,
                parent_remaining,
            });
        }

        let cost_micros_allocated = if parent_cost_remaining == 0 {
            0
        } else {
            ((parent_cost_remaining as f64 * fraction) as u64)
                .max(1)
                .min(parent_cost_remaining)
        };
        let child_budget = WorkNodeBudget {
            tokens_allocated: cascaded.allocated_tokens,
            tokens_consumed: 0,
            cost_micros_allocated,
            cost_micros_consumed: 0,
        };
        let chain_id = self
            .chain_id_for_node(parent_id)
            .expect("parent node always has a root node");
        self.pending_child_budgets
            .insert(child_id.to_string(), child_budget.clone());
        self.record_chain_allocation(
            &chain_id,
            0,
            child_budget.tokens_allocated,
            parent_depth + 1,
        );

        Ok(child_budget)
    }

    /// Mark a node as completed with an outcome reference.
    pub(crate) fn complete(
        &mut self,
        node_id: &str,
        outcome_ref: String,
    ) -> Result<(), WorkGraphError> {
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        if node.status != NodeStatus::Active {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        }
        node.status = NodeStatus::Completed;
        node.outcome_ref = Some(outcome_ref);
        Ok(())
    }

    /// Stop a node and all its descendants (cascade).
    pub(crate) fn stop(
        &mut self,
        node_id: &str,
        reason: StopReason,
    ) -> Result<Vec<String>, WorkGraphError> {
        if !self.nodes.contains_key(node_id) {
            return Err(WorkGraphError::NodeNotFound(node_id.to_string()));
        }
        let mut stopped = Vec::new();
        self.stop_recursive(node_id, reason, &mut stopped);
        Ok(stopped)
    }

    /// Record token consumption on a node.
    pub(crate) fn consume_budget(
        &mut self,
        node_id: &str,
        tokens: u64,
        cost_micros: u64,
    ) -> Result<bool, WorkGraphError> {
        let exhausted = {
            let node = self
                .nodes
                .get_mut(node_id)
                .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
            if node.status != NodeStatus::Active {
                return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
            }
            node.budget.tokens_consumed = node.budget.tokens_consumed.saturating_add(tokens);
            node.budget.cost_micros_consumed =
                node.budget.cost_micros_consumed.saturating_add(cost_micros);
            node.budget.is_exhausted()
        };
        let chain_id = self
            .chain_id_for_node(node_id)
            .expect("node always has a root node");
        self.ensure_chain_budget(&chain_id);
        let chain_exhausted = {
            let chain = self
                .chain_budgets
                .get_mut(&chain_id)
                .expect("chain budget initialized");
            chain.total_consumed_tokens = chain.total_consumed_tokens.saturating_add(tokens);
            chain.total_consumed_tokens >= chain.root_budget_tokens
        };

        if chain_exhausted {
            self.stop_recursive(&chain_id, StopReason::BudgetExhausted, &mut Vec::new());
            return Ok(true);
        }
        if exhausted {
            let node = self.nodes.get_mut(node_id).expect("node checked above");
            node.status = NodeStatus::Stopped;
            node.stop_reason = Some(StopReason::BudgetExhausted);
            return Ok(true);
        }

        Ok(false)
    }

    /// Records token consumption for a node and updates its chain budget.
    pub(crate) fn consume_tokens(
        &mut self,
        node_id: &str,
        tokens: u64,
    ) -> Result<(), WorkGraphError> {
        self.consume_budget(node_id, tokens, 0)?;
        Ok(())
    }

    /// Returns the chain budget for a given node's chain.
    pub(crate) fn chain_budget_for(&self, node_id: &str) -> Option<&ChainBudget> {
        let chain_id = self.chain_id_for_node(node_id)?;
        self.chain_budgets.get(&chain_id)
    }

    /// Returns all chains at or above their utilization threshold.
    pub(crate) fn over_budget_chains(&self, threshold_pct: f64) -> Vec<&ChainBudget> {
        self.chain_budgets
            .values()
            .filter(|budget| budget.utilization_pct() >= threshold_pct)
            .collect()
    }

    /// Check all nodes for stop conditions and cascade.
    pub(crate) fn enforce_stop_conditions(&mut self) -> Vec<(String, StopReason)> {
        let exhausted: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.status == NodeStatus::Active && n.budget.is_exhausted())
            .map(|(id, _)| id.clone())
            .collect();
        let mut stopped = Vec::new();
        for node_id in exhausted {
            let mut cascade = Vec::new();
            self.stop_recursive(&node_id, StopReason::BudgetExhausted, &mut cascade);
            for id in cascade {
                stopped.push((id, StopReason::BudgetExhausted));
            }
        }
        stopped
    }

    pub(crate) fn get_node(&self, node_id: &str) -> Option<&WorkNode> {
        self.nodes.get(node_id)
    }

    pub(crate) fn children_of(&self, node_id: &str) -> &[String] {
        self.children.get(node_id).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn active_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.status == NodeStatus::Active)
            .count()
    }

    pub(crate) fn total_count(&self) -> usize {
        self.nodes.len()
    }

    #[allow(clippy::collapsible_if)]
    fn stop_recursive(&mut self, node_id: &str, reason: StopReason, stopped: &mut Vec<String>) {
        if let Some(node) = self.nodes.get_mut(node_id) {
            if matches!(node.status, NodeStatus::Active | NodeStatus::Pending) {
                node.status = NodeStatus::Stopped;
                node.stop_reason = Some(reason);
                stopped.push(node_id.to_string());
            }
        }
        let children: Vec<String> = self.children.get(node_id).cloned().unwrap_or_default();
        for child_id in children {
            self.stop_recursive(&child_id, StopReason::ParentStopped, stopped);
        }
    }

    fn chain_id_for_node(&self, node_id: &str) -> Option<String> {
        let mut current_id = node_id;
        let mut current = self.nodes.get(current_id)?;
        while let Some(parent_id) = current.parent_node_id.as_deref() {
            current_id = parent_id;
            current = self.nodes.get(current_id)?;
        }
        Some(current_id.to_string())
    }

    fn node_lineage(&self, node_id: &str) -> Vec<String> {
        let mut lineage = Vec::new();
        let mut current_id = Some(node_id);
        while let Some(id) = current_id {
            let Some(node) = self.nodes.get(id) else {
                break;
            };
            lineage.push(id.to_string());
            current_id = node.parent_node_id.as_deref();
        }
        lineage.reverse();
        lineage
    }

    fn ensure_chain_budget(&mut self, chain_id: &str) {
        let root_budget_tokens = self
            .nodes
            .get(chain_id)
            .map_or(0, |node| node.budget.tokens_allocated);
        self.chain_budgets
            .entry(chain_id.to_string())
            .or_insert(ChainBudget {
                chain_id: chain_id.to_string(),
                root_budget_tokens,
                total_consumed_tokens: 0,
                total_allocated_tokens: root_budget_tokens,
                depth: 0,
            });
    }

    fn record_chain_allocation(
        &mut self,
        chain_id: &str,
        previous_tokens: u64,
        tokens_allocated: u64,
        depth: u16,
    ) {
        self.ensure_chain_budget(chain_id);
        let chain = self
            .chain_budgets
            .get_mut(chain_id)
            .expect("chain budget initialized");
        chain.total_allocated_tokens = chain
            .total_allocated_tokens
            .saturating_sub(previous_tokens)
            .saturating_add(tokens_allocated);
        chain.depth = chain.depth.max(depth);
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkGraphError {
    #[error("graph at capacity ({MAX_GRAPH_NODES} nodes)")]
    CapacityExceeded,
    #[error("duplicate node: {0}")]
    DuplicateNode(String),
    #[error("node not found: {0}")]
    NodeNotFound(String),
    #[error("parent not active: {0}")]
    ParentNotActive(String),
    #[error("depth exceeds max {0}")]
    DepthExceeded(u16),
    #[error("fan-out exceeds max {0}")]
    FanOutExceeded(usize),
    #[error("child budget ({child_requested}) exceeds parent remaining ({parent_remaining})")]
    BudgetExceedsParent {
        child_requested: u64,
        parent_remaining: u64,
    },
    #[error("invalid status transition for node: {0}")]
    InvalidTransition(String),
    #[error("budget cascade error: {0}")]
    Cascade(#[from] CascadeError),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::core::work_graph::{
        BoundedWorkGraph, MAX_DEPTH, MAX_FAN_OUT, NodeStatus, StopReason, WorkGraphError,
        WorkNodeBudget,
    };

    fn budget(tokens: u64, cost: u64) -> WorkNodeBudget {
        WorkNodeBudget {
            tokens_allocated: tokens,
            tokens_consumed: 0,
            cost_micros_allocated: cost,
            cost_micros_consumed: 0,
        }
    }

    #[test]
    fn basic_delegation_and_budget_inheritance() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "parent-agent".into(),
            "capsule:abc".into(),
            budget(1000, 500),
        )
        .unwrap();
        g.delegate(
            "root",
            "child-1".into(),
            "child-agent".into(),
            "capsule:def".into(),
            budget(400, 200),
        )
        .unwrap();
        assert_eq!(g.active_count(), 2);
        assert_eq!(g.children_of("root"), &["child-1"]);
    }

    #[test]
    fn child_cannot_exceed_parent_budget() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:x".into(),
            budget(100, 50),
        )
        .unwrap();
        assert!(matches!(
            g.delegate(
                "root",
                "c".into(),
                "b".into(),
                "capsule:y".into(),
                budget(200, 30)
            ),
            Err(WorkGraphError::BudgetExceedsParent { .. })
        ));
    }

    #[test]
    fn fan_out_limit_enforced() {
        let mut g = BoundedWorkGraph::new(2, MAX_DEPTH);
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:x".into(),
            budget(1000, 1000),
        )
        .unwrap();
        g.delegate(
            "root",
            "c1".into(),
            "b".into(),
            "capsule:1".into(),
            budget(100, 100),
        )
        .unwrap();
        g.delegate(
            "root",
            "c2".into(),
            "b".into(),
            "capsule:2".into(),
            budget(100, 100),
        )
        .unwrap();
        assert!(matches!(
            g.delegate(
                "root",
                "c3".into(),
                "b".into(),
                "capsule:3".into(),
                budget(100, 100)
            ),
            Err(WorkGraphError::FanOutExceeded(2))
        ));
    }

    #[test]
    fn depth_limit_enforced() {
        let mut g = BoundedWorkGraph::new(MAX_FAN_OUT, 2);
        g.add_root(
            "n0".into(),
            "a".into(),
            "capsule:0".into(),
            budget(1000, 1000),
        )
        .unwrap();
        g.delegate(
            "n0",
            "n1".into(),
            "b".into(),
            "capsule:1".into(),
            budget(500, 500),
        )
        .unwrap();
        g.delegate(
            "n1",
            "n2".into(),
            "c".into(),
            "capsule:2".into(),
            budget(200, 200),
        )
        .unwrap();
        assert!(matches!(
            g.delegate(
                "n2",
                "n3".into(),
                "d".into(),
                "capsule:3".into(),
                budget(100, 100)
            ),
            Err(WorkGraphError::DepthExceeded(2))
        ));
    }

    #[test]
    fn stop_cascades_to_children() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(1000, 1000),
        )
        .unwrap();
        g.delegate(
            "root",
            "c1".into(),
            "b".into(),
            "capsule:1".into(),
            budget(300, 300),
        )
        .unwrap();
        g.delegate(
            "c1",
            "gc1".into(),
            "c".into(),
            "capsule:gc".into(),
            budget(100, 100),
        )
        .unwrap();
        let stopped = g.stop("c1", StopReason::Stale).unwrap();
        assert_eq!(stopped, vec!["c1", "gc1"]);
        assert_eq!(g.get_node("c1").unwrap().status, NodeStatus::Stopped);
        assert_eq!(
            g.get_node("gc1").unwrap().stop_reason,
            Some(StopReason::ParentStopped)
        );
    }

    #[test]
    fn budget_exhaustion_auto_stops() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(100, 100),
        )
        .unwrap();
        let exhausted = g.consume_budget("root", 100, 50).unwrap();
        assert!(exhausted);
        assert_eq!(g.get_node("root").unwrap().status, NodeStatus::Stopped);
    }

    #[test]
    fn allocate_child_budget_uses_requested_fraction() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(10_000, 5_000),
        )
        .unwrap();

        let child_budget = g.allocate_child_budget("root", "child", 0.5).unwrap();

        assert_eq!(child_budget, budget(5_000, 2_500));
        let chain = g.chain_budget_for("root").unwrap();
        assert_eq!(chain.total_allocated_tokens, 15_000);
        assert_eq!(chain.depth, 1);
    }

    #[test]
    fn allocate_child_budget_rejects_exhausted_parent() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(100, 100),
        )
        .unwrap();
        g.consume_tokens("root", 100).unwrap();

        assert!(g.allocate_child_budget("root", "child", 0.5).is_err());
    }

    #[test]
    fn consume_tokens_updates_node_and_chain() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(1_000, 1_000),
        )
        .unwrap();

        g.consume_tokens("root", 250).unwrap();

        assert_eq!(g.get_node("root").unwrap().budget.tokens_consumed, 250);
        assert_eq!(
            g.chain_budget_for("root").unwrap().total_consumed_tokens,
            250
        );
    }

    #[test]
    fn consume_tokens_stops_exhausted_node() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(100, 100),
        )
        .unwrap();

        g.consume_tokens("root", 100).unwrap();

        let node = g.get_node("root").unwrap();
        assert_eq!(node.status, NodeStatus::Stopped);
        assert_eq!(node.stop_reason, Some(StopReason::BudgetExhausted));
    }

    #[test]
    fn over_budget_chains_returns_chains_above_threshold() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "first".into(),
            "a".into(),
            "capsule:first".into(),
            budget(1_000, 1_000),
        )
        .unwrap();
        g.add_root(
            "second".into(),
            "b".into(),
            "capsule:second".into(),
            budget(1_000, 1_000),
        )
        .unwrap();
        g.consume_tokens("first", 750).unwrap();

        let over_budget = g.over_budget_chains(70.0);

        assert_eq!(over_budget.len(), 1);
        assert_eq!(over_budget[0].chain_id, "first");
    }

    #[test]
    fn complete_sets_outcome() {
        let mut g = BoundedWorkGraph::default();
        g.add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(1000, 1000),
        )
        .unwrap();
        g.complete("root", "outcome:success".into()).unwrap();
        let node = g.get_node("root").unwrap();
        assert_eq!(node.status, NodeStatus::Completed);
        assert_eq!(node.outcome_ref.as_deref(), Some("outcome:success"));
    }
}
