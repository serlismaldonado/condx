// Sondas de estado (§4.1): cada una corre en su propio bucle, guarda el último
// resultado y sigue aunque falle (un `docker` caído no frena a git o al CI).

pub mod ci;
pub mod convex;
mod docker;
pub mod git;
mod vercel;

use std::future::Future;
use std::time::Duration;

use crate::state::Shared;

pub use convex::{env_var_names, function_logs};
pub use docker::docker_container;
pub use git::commits_between;

/// Arranca todas las sondas.
pub fn spawn_all(state: &Shared) {
    every(state, Duration::from_secs(5), docker::containers);
    every(state, Duration::from_secs(4), docker::deployer);
    every(state, Duration::from_secs(15), git::repos);
    every_dyn(state, ci::runs);
    every(state, Duration::from_secs(30), convex::environments);
    every(state, Duration::from_secs(120), vercel::deployments);
}

fn every<F, Fut>(state: &Shared, period: Duration, probe: F)
where
    F: Fn(Shared) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            probe(state.clone()).await;
        }
    });
}

/// Como `every`, pero la sonda decide cuánto esperar (CI: más seguido si hay
/// corridas en curso).
fn every_dyn<F, Fut>(state: &Shared, probe: F)
where
    F: Fn(Shared) -> Fut + Send + 'static,
    Fut: Future<Output = Duration> + Send,
{
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            let wait = probe(state.clone()).await;
            tokio::time::sleep(wait).await;
        }
    });
}
