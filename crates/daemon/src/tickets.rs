//! Tickets efímeros para el WebSocket remoto.
//!
//! El handshake WS del navegador no admite `Authorization` y el BFF de Vercel
//! (serverless) no proxya WebSockets, así que el cliente remoto autenticado
//! pide por REST un ticket de un solo uso y abre el WS directo al daemon con
//! `?ticket=`. Los tickets viven **en memoria del proceso** (instancia única
//! en Railway): 32 bytes aleatorios, TTL corto, consumidos al primer uso.
//! La tabla `auth_tickets` de Postgres queda reservada para cuando haya más
//! de una instancia.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::time::{Duration, Instant};

/// Vida de un ticket: suficiente para un handshake, inútil para robo tardío.
pub const TICKET_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct TicketStore {
    /// ticket → expiración. Valores de 256 bits aleatorios: la búsqueda por
    /// HashMap no es oráculo de timing útil contra ese espacio.
    pending: Mutex<HashMap<String, Instant>>,
}

impl TicketStore {
    /// Emite un ticket nuevo con el TTL estándar.
    pub fn issue(&self) -> (String, u32) {
        self.issue_with_ttl(TICKET_TTL)
    }

    /// Variante con TTL explícito (tests de expiración).
    pub fn issue_with_ttl(&self, ttl: Duration) -> (String, u32) {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let ticket = URL_SAFE_NO_PAD.encode(bytes);
        let mut pending = self.pending.lock().expect("tickets mutex");
        let now = Instant::now();
        // Poda de caducados: el mapa no crece con tickets abandonados.
        pending.retain(|_, expiry| *expiry > now);
        pending.insert(ticket.clone(), now + ttl);
        (ticket, ttl.as_secs() as u32)
    }

    /// Consume el ticket: válido exactamente una vez y solo antes de caducar.
    pub fn consume(&self, ticket: &str) -> bool {
        let mut pending = self.pending.lock().expect("tickets mutex");
        match pending.remove(ticket) {
            Some(expiry) => expiry > Instant::now(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn un_solo_uso() {
        let store = TicketStore::default();
        let (ticket, ttl) = store.issue();
        assert_eq!(ttl, 60);
        assert!(store.consume(&ticket), "primer uso válido");
        assert!(!store.consume(&ticket), "segundo uso rechazado");
    }

    #[tokio::test]
    async fn desconocido_rechazado() {
        let store = TicketStore::default();
        assert!(!store.consume("no-existe"));
    }

    #[tokio::test(start_paused = true)]
    async fn expirado_rechazado() {
        let store = TicketStore::default();
        let (ticket, _) = store.issue();
        tokio::time::advance(TICKET_TTL + Duration::from_secs(1)).await;
        assert!(!store.consume(&ticket));
    }
}
