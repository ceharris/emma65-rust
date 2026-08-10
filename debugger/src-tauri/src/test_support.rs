//! Shared test-only helpers for modules whose tests mutate process-global
//! state (currently just the `HOME` env var).

use std::sync::Mutex;

/// Serializes tests across modules that temporarily override the
/// process-global `HOME` env var, since `cargo test` runs tests in parallel
/// threads within one process and `HOME` is shared by every thread.
///
/// Each module's own tests must acquire this lock (not a private one) before
/// calling `std::env::set_var("HOME", ..)`, or a test in another module can
/// race it and see the wrong `HOME`.
pub(crate) static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());
