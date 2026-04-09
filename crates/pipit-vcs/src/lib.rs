//! # pipit-vcs — VCS Workflow Kernel
//!
//! Single source of truth for all repository mutations. This crate provides:
//!
//! - **Workflow FSM** — typed state machine for repository lifecycle operations
//! - **Snapshot Graph** — immutable DAG of workspace snapshots with provenance
//! - **Semantic Git Firewall** — deep validation of Git operations against trust boundaries
//! - **Workspace Reconciliation** — two-phase protocol for safe workspace disposal
//! - **Branch Contracts** — typed execution constraints with promotion gates
//! - **Repository Ledger** — append-only event log for all VCS mutations
//!
//! Design invariant: every repository mutation flows through this kernel.
//! No raw `git` command should be executed outside this module.

pub mod contract;
pub mod firewall;
pub mod gateway;
pub mod ledger;
pub mod reconcile;
pub mod snapshot;
pub mod workflow;

pub use contract::{BranchContract, ContractPredicate, GateResult, PromotionGate};
pub use firewall::{FirewallDecision, GitFirewall, ThreatClass};
pub use gateway::{GatewayError, VcsGateway};
pub use ledger::{LedgerEntry, LedgerEvent, RepositoryLedger};
pub use reconcile::{ReconcileAction, ReconcileOutcome, WorkspaceReconciler, WorkspaceState};
pub use snapshot::{Snapshot, SnapshotGraph, SnapshotId};
pub use workflow::{VcsKernel, WorkflowOp, WorkflowPhase, WorkflowTransition};
