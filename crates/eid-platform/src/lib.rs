//! OS abstraction layer (ADR-0006). **All** OS-specific code and **all** process spawning
//! lives here; a guard test (`tests/guard.rs`) enforces that nothing else uses
//! `cfg(windows)`, `std::process::Command` or raw sockets.
//!
//! Windows implementations are compile-checked on Linux (`--target x86_64-pc-windows-gnu`)
//! but NOT executed here: see docs/STATUS.md "not tested on real Windows".

pub mod dirs;
pub mod fs;
pub mod ipc;
pub mod native_host;
pub mod net;
pub mod process;
pub mod time;
pub const IS_WINDOWS: bool = cfg!(windows);
/// Longest full path (in UTF-16 code units, without the terminating NUL) that ordinary file APIs
/// accept: Windows `MAX_PATH` = 260. Windows only works beyond it with `\\?\` paths plus a
/// `longPathAware` manifest, neither of which exists yet (planned P7/P8, MANUAL-TESTS #5), so the
/// engine keeps every path it creates within this limit. `None` elsewhere (PATH_MAX is 4096).
pub const MAX_PATH_UNITS: Option<usize> = if IS_WINDOWS { Some(259) } else { None };
