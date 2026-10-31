//! SMTP server for Spoolbox (RFC 5321 with SIZE, PIPELINING, 8BITMIME, SMTPUTF8,
//! ENHANCEDSTATUSCODES, STARTTLS and AUTH PLAIN/LOGIN).
//!
//! The protocol logic ([`session`], [`data`], [`command`]) performs no I/O; [`server`]
//! drives it over TCP. Storage is abstracted by [`Ingest`].

pub mod command;
pub mod data;
pub mod server;
pub mod session;
pub mod tls;

pub use server::{BodyInfo, Ingest, Server, ServerOptions};
pub use session::{RecipientPolicy, SessionConfig, SmtpAuth};
pub use spoolbox_core::chaos::{Chaos, Triggers as ChaosTriggers};
