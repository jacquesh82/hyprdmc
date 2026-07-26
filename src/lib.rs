//! `hyprmc` — gestion dynamique des écrans sous Hyprland.
//!
//! Le découpage suit le flux de données : `ipc` parle au compositeur, `monitor`
//! modélise ce qu'il rapporte, `layout` calcule et valide un agencement,
//! `apply` l'applique avec filet de sécurité, `config` et `emit` le persistent,
//! `daemon` réagit au branchement à chaud et `web` expose le tout.

pub mod apply;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod emit;
pub mod ipc;
pub mod layout;
pub mod monitor;
pub mod web;
