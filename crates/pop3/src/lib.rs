//! POP3 server for Spoolbox (RFC 1939 with TOP and UIDL, RFC 2449 CAPA, RFC 2595 STLS).
//!
//! The protocol logic ([`session`]) performs no I/O; [`server`] drives it over TCP. Storage is
//! abstracted by [`Maildrop`]. Every user sees the same mailbox.

pub mod server;
pub mod session;

pub use server::{Maildrop, Server, ServerOptions};
pub use session::{Entry, Session, SessionConfig, Step, encode};
