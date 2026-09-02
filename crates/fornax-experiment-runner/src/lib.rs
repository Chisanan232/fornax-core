//! Bounded execution boundary for FORNX-99's counterfactual experiment
//! contract (FORNX-100, epic FORNX-20 / discovery thesis HVDL-15).
//!
//! `fornax-types::experiment` (FORNX-99) defines what an experiment *is* —
//! [`fornax_types::experiment::ExperimentSpec`] in,
//! [`fornax_types::experiment::ExperimentOutcome`] out — as a pure,
//! serializable contract with no executor, sandbox, or isolation mechanism
//! of its own. This crate is that executor: given a validated spec, it
//! actually runs it, inside an ephemeral, isolated boundary the primary
//! working tree can never see, and produces the outcome. See this crate's
//! individual modules (added incrementally in subsequent commits) for the
//! staging, orphan-cleanup, and execution pieces.

pub mod policy;

pub use policy::{is_permitted, ExperimentPolicyError, GlobalExperimentPolicy};
