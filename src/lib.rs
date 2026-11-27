//! Core of ccmon: read Claude Code transcripts and build a live model of sessions,
//! subagents and workflows. Shared by the TUI (`ccmon`) and the GUI (`gui/`).

pub mod bgtask;
pub mod catalog;
pub mod codex;
pub mod config;
pub mod control;
pub mod discovery;
pub mod event;
pub mod metrics;
pub mod platform;
pub mod setup;
pub mod store;
pub mod tail;
pub mod tasktail;
pub mod tee;
pub mod thread;
