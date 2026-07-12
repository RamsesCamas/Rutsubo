//! Enrutamiento en memoria por cuenta (ADR-006): a lo sumo un daemon y N
//! suscriptores por cuenta. El relay no persiste tránsito (RNF-10): si no hay
//! daemon conectado, el mensaje del cliente se responde con
//! `daemon_unavailable` y se descarta.
//!
//! Las entradas se indexan por `conn_id` (único por conexión, no por
//! dispositivo): si un dispositivo reconecta antes de que su socket viejo
//! muera, la limpieza del viejo no debe borrar el registro del nuevo.

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Cierre C-2: un segundo daemon de la misma cuenta desplaza al anterior.
pub const CLOSE_SUPERSEDED: u16 = 4001;
/// Cierre C-2: conexión sin pong en 90 s.
pub const CLOSE_IDLE: u16 = 4002;

pub type Tx = mpsc::Sender<Message>;

#[derive(Default)]
struct AccountHub {
    /// `(conn_id, device_id, canal de salida)` del daemon conectado.
    daemon: Option<(String, String, Tx)>,
    /// Suscriptores activos: conn_id → (device_id, canal de salida).
    subscribers: HashMap<String, (String, Tx)>,
}

#[derive(Default)]
pub struct Hub {
    accounts: Mutex<HashMap<String, AccountHub>>,
}

impl Hub {
    /// Registra el daemon de la cuenta. Si había otro, lo devuelve para que
    /// el llamador le mande el close 4001 (`superseded`).
    pub fn register_daemon(
        &self,
        account_id: &str,
        conn_id: &str,
        device_id: &str,
        tx: Tx,
    ) -> Option<Tx> {
        let mut accounts = self.accounts.lock().unwrap();
        let hub = accounts.entry(account_id.to_owned()).or_default();
        let previous = hub
            .daemon
            .replace((conn_id.to_owned(), device_id.to_owned(), tx));
        previous.map(|(_, _, tx)| tx)
    }

    /// Da de baja el daemon solo si esta conexión sigue siendo la registrada
    /// (una conexión desplazada no debe borrar a su sucesora).
    pub fn unregister_daemon(&self, account_id: &str, conn_id: &str) {
        let mut accounts = self.accounts.lock().unwrap();
        if let Some(hub) = accounts.get_mut(account_id)
            && hub.daemon.as_ref().is_some_and(|(id, _, _)| id == conn_id)
        {
            hub.daemon = None;
        }
    }

    pub fn register_subscriber(&self, account_id: &str, conn_id: &str, device_id: &str, tx: Tx) {
        let mut accounts = self.accounts.lock().unwrap();
        accounts
            .entry(account_id.to_owned())
            .or_default()
            .subscribers
            .insert(conn_id.to_owned(), (device_id.to_owned(), tx));
    }

    pub fn unregister_subscriber(&self, account_id: &str, conn_id: &str) {
        let mut accounts = self.accounts.lock().unwrap();
        if let Some(hub) = accounts.get_mut(account_id) {
            hub.subscribers.remove(conn_id);
        }
    }

    /// Canal hacia el daemon de la cuenta, si hay uno conectado.
    pub fn daemon_tx(&self, account_id: &str) -> Option<Tx> {
        let accounts = self.accounts.lock().unwrap();
        accounts
            .get(account_id)
            .and_then(|hub| hub.daemon.as_ref())
            .map(|(_, _, tx)| tx.clone())
    }

    /// Difunde un frame C-3 a todos los suscriptores de la cuenta.
    pub fn broadcast(&self, account_id: &str, frame: &str) {
        let txs: Vec<Tx> = {
            let accounts = self.accounts.lock().unwrap();
            accounts
                .get(account_id)
                .map(|hub| hub.subscribers.values().map(|(_, tx)| tx.clone()).collect())
                .unwrap_or_default()
        };
        for tx in txs {
            // try_send: un suscriptor saturado pierde frames y repondrá por
            // seq al reconectar (C-3); no puede frenar al resto de la cuenta.
            let _ = tx.try_send(Message::Text(Utf8Bytes::from(frame.to_owned())));
        }
    }

    /// Entrega un frame C-3 a las conexiones de un dispositivo concreto
    /// (backlog unicast de `subscribe_session`).
    pub fn send_to(&self, account_id: &str, device_id: &str, frame: &str) {
        let txs: Vec<Tx> = {
            let accounts = self.accounts.lock().unwrap();
            accounts
                .get(account_id)
                .map(|hub| {
                    hub.subscribers
                        .values()
                        .filter(|(dev, _)| dev == device_id)
                        .map(|(_, tx)| tx.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        for tx in txs {
            let _ = tx.try_send(Message::Text(Utf8Bytes::from(frame.to_owned())));
        }
    }

    /// Cierra las conexiones vivas de un dispositivo revocado (daemon o cliente).
    pub fn disconnect_device(&self, account_id: &str, device_id: &str) {
        let txs: Vec<Tx> = {
            let mut accounts = self.accounts.lock().unwrap();
            let Some(hub) = accounts.get_mut(account_id) else {
                return;
            };
            let mut txs = Vec::new();
            if hub
                .daemon
                .as_ref()
                .is_some_and(|(_, dev, _)| dev == device_id)
                && let Some((_, _, tx)) = hub.daemon.take()
            {
                txs.push(tx);
            }
            hub.subscribers.retain(|_, (dev, tx)| {
                if dev == device_id {
                    txs.push(tx.clone());
                    false
                } else {
                    true
                }
            });
            txs
        };
        for tx in txs {
            let _ = tx.try_send(close_frame(CLOSE_SUPERSEDED, "revoked"));
        }
    }
}

pub fn close_frame(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }))
}
