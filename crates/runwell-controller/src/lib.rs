//! Controller-side placement, GitHub lifecycle and fleet resilience.
//! Host operations use the same NodeBackend port as standalone operation.
mod backend;
pub mod bootstrap;
mod fleet;
mod orphans;
mod production;
mod releases;
mod resilience;
mod scheduling;
mod streams;
pub use fleet::{Fleet, Hooks, LogHooks};
pub use runwell_node::{Controller, GithubGateway, RunnerApi, run_loop};

mod alerts;
pub mod execution;
pub mod monitoring;
mod observations;
mod telemetry;
pub use telemetry::Telemetry;
