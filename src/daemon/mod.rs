//! Local HTTP daemon — single registry instance for CLI, TUI, and GUI.

mod server;

pub use server::serve;
