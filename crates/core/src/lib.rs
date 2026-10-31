//! Domain types shared by all Spoolbox crates. This crate performs no I/O.

pub mod chaos;
pub mod envelope;
pub mod listen;
pub mod secret;

pub use envelope::Envelope;
pub use listen::{ListenAddr, ListenAddrError, ListenHost};
pub use secret::Secret;

/// Crate version of the Spoolbox workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
