//! OS abstraction layer (ADR-0006). **All** OS-specific code and **all** process spawning
//! lives here; a guard test (`tests/guard.rs`) enforces that nothing else uses
//! `cfg(windows)`, `std::process::Command` or raw sockets.
//!
//! Windows implementations are compile-checked on Linux (`--target x86_64-pc-windows-gnu`)
//! but NOT executed here: see docs/STATUS.md "not tested on real Windows".

pub mod fs;
pub mod process;
pub const IS_WINDOWS: bool = cfg!(windows);
