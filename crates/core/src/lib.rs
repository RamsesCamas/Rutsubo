//! Contrato único del protocolo Rutsubo (ADR-004).
//!
//! Este crate es la fuente de verdad de todo lo que viaja entre el daemon y
//! sus clientes: el sobre de eventos (contrato C-3), el catálogo de eventos y
//! comandos, la validación de rutas del workspace (RNF-05) y la representación
//! de diffs (RF-27). Los tipos TypeScript del cliente web se generan desde
//! aquí con `ts-rs` (`just bindings`); los esquemas JSON de los documentos de
//! diseño son proyección legible de estos tipos.

pub mod commands;
pub mod diff;
pub mod envelope;
pub mod events;
pub mod ids;
pub mod paths;

pub use commands::Command;
pub use envelope::{Envelope, PROTOCOL_VERSION};
pub use events::Event;
