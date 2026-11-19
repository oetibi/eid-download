//! Eid Download desktop app: a Tauri 2 window over the same engine, queue and IPC endpoint as
//! `eid serve`. [`core`] is the logic (unit tested everywhere); [`app`] wires it to Tauri.

pub mod app;
pub mod core;
pub mod video;
