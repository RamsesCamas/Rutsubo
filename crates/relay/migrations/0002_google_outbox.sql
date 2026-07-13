-- Identidad Google (C-2 enmendado): la cuenta se ancla al claim `sub` de
-- Google. `password_hash` deja de usarse (se guarda '' para cuentas Google);
-- SQLite no permite volver la columna nullable por ALTER, así que se conserva
-- NOT NULL con placeholder.
ALTER TABLE accounts ADD COLUMN google_sub TEXT;
CREATE UNIQUE INDEX idx_accounts_google_sub ON accounts(google_sub);

-- Plataforma del dispositivo (mobile|desktop|web); `kind` sigue siendo
-- client|daemon (distingue el daemon pareado).
ALTER TABLE devices ADD COLUMN platform TEXT;

-- Buzón de tareas offline (ADR-009). ÚNICA excepción de persistencia de
-- contenido del relay (RNF-10): en M2 el payload va en claro → el relay no
-- sale de LAN hasta el cifrado sealed-box de M3.
CREATE TABLE outbox (
    id                TEXT PRIMARY KEY,             -- ULID
    account_id        TEXT NOT NULL REFERENCES accounts(id),
    enqueued_by       TEXT NOT NULL,                -- device_id que encoló
    target_session_id TEXT,                         -- NULL = crear sesión nueva
    new_session_title TEXT,
    payload_kind      TEXT NOT NULL,                -- plaintext | sealed_box
    payload           BLOB NOT NULL,
    client_msg_id     TEXT NOT NULL,
    state             TEXT NOT NULL DEFAULT 'queued', -- queued|delivered|expired
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    UNIQUE(account_id, client_msg_id)
);
CREATE INDEX idx_outbox_account ON outbox(account_id, created_at);
