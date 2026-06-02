//! TurnPipeline — structured event handling for the main loop.
//!
//! Three layers: **handlers** (pure functions → `Vec<PipelineAction>`),
//! **executor** (maps actions to component I/O), and the **orchestrator**
//! (`tokio::select!` in `main.rs` — fetches data, calls handlers, feeds
//! actions to the executor).

pub mod executor;
pub mod handlers;
pub mod types;

pub use executor::{ActionExecutor, PipelineComponents};
pub use types::{PipelineAction, TurnState};
