//! Core types shared by every Kiln crate.
//!
//! Nothing here knows about HTTP, the terminal, or any model vendor.

pub mod config;
pub mod error;
pub mod message;
pub mod paths;
pub mod permission;
pub mod session;

pub use config::Config;
pub use error::{Error, Result};
pub use message::{Message, ToolCall};
pub use permission::{Mode, Risk};
pub use session::Session;
