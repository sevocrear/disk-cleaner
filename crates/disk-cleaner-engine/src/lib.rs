//! Disk cleaner engine — ported from host_cleaner.py with GUI extras.

pub mod action;
pub mod cmd;
pub mod config;
pub mod disks;
pub mod execute;
pub mod phases;
pub mod report;
pub mod run;
pub mod safety;
pub mod trash;
pub mod util;

pub use action::{Action, ApplyProgress, ApplySummary, PhaseResult};
pub use config::Config;
pub use execute::{default_apply_jobs, execute_actions, execute_actions_parallel};
pub use run::{run_phases, run_phases_parallel, should_run_phase, PHASE_ORDER};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
