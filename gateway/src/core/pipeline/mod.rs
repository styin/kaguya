//! TurnPipeline — structured event handling for the main loop.
//!
//! Three layers: **handlers** (pure functions → `Vec<PipelineAction>`),
//! **executor** (maps actions to component I/O), and the **orchestrator**
//! ([`run`] — fetches data, calls handlers, feeds
//! actions to the executor).

pub mod executor;
pub mod handlers;
mod run;
pub mod types;

pub use executor::{ActionExecutor, PipelineComponents};
pub use run::run;
pub use types::{PipelineAction, TurnState};
