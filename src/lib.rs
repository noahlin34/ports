//! Shared domain model and filtering primitives for the `ports` application.
//!
//! The command-line and terminal interfaces intentionally depend on these modules
//! rather than maintaining separate representations of sockets and processes.

pub mod discovery;
pub mod filter;
pub mod model;
