//! Codec-agnostic data model shared by the analysis backend and the UI.
//!
//! Nothing in this crate depends on FFI, decoders, or iced.

pub mod analysis;
pub mod frame;
pub mod stream;

pub use analysis::*;
pub use frame::*;
pub use stream::*;
