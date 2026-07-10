//! Daemon de Rutsubo: API REST (C-1), WebSocket de eventos (C-3), agent loop
//! con compuerta de permisos y adapter LLM (C-4).
//!
//! Expuesto como librería para los tests de integración; el binario
//! (`main.rs`) solo hace el arranque.

pub mod agent;
pub mod api;
pub mod asr;
pub mod auth;
pub mod config;
pub mod error;
pub mod gate;
pub mod llm;
pub mod state;
pub mod store;
pub mod tickets;
pub mod tools;
pub mod ws;
