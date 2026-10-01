//! Controller-side placement, GitHub lifecycle and fleet resilience.
//! Host operations use the same NodeBackend port as standalone operation.
mod backend;
pub mod bootstrap;
mod fleet;
mod orphans;
mod releases;
mod resilience;
mod scheduling;
mod streams;
pub use fleet::{Fleet, Hooks, LogHooks};
pub use runwell_node::{Controller, GithubGateway, RunnerApi, run_loop};
