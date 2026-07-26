//! `hyprdmc` — Hypr Dynamic Monitor Configuration.
//!
//! The modules follow the flow of data: `ipc` talks to the compositor,
//! `monitor` models what it reports, `layout` computes and validates an
//! arrangement, `apply` pushes it with a safety net, `config` and `emit`
//! persist it, `daemon` reacts to hotplug, and `web` exposes the whole thing.
//!
//! Every user-facing string goes through `t!()`; see [`i18n`].

rust_i18n::i18n!("locales", fallback = "en");

pub mod apply;
pub mod browser;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod emit;
pub mod history;
pub mod i18n;
pub mod ipc;
pub mod layout;
pub mod monitor;
pub mod notify;
pub mod web;
