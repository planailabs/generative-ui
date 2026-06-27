//! `gui-server` library: surface discovery + (later stages) HTTP serving and backflow.

pub mod discovery;

pub use discovery::{Agent, Deck, Index, Surface, SurfaceVersion};
