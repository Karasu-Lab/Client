//! SteelMC inside the client's own process.
//!
//! [`generated`] runs terrain generation alone, with no server: it is what the
//! web build uses. The `server` feature adds the integrated server behind
//! singleplayer, which joins a real server over an in-memory pipe rather than a
//! socket.

pub mod generated;

#[cfg(feature = "server")]
mod server;
#[cfg(feature = "server")]
pub use server::*;
