//! `gui-server` library: discover surfaces from the artifacts directory, serve them as
//! interactive HTML, and (later stages) route backflow and stream live updates.

pub mod api;
pub mod discovery;
pub mod render;
pub mod server;

pub use discovery::{Agent, Deck, Index, Surface, SurfaceVersion};
pub use server::{router, serve, AppState};
