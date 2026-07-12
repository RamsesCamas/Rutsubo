-- Esquema COMPLETO de persistencia del relay (frontera de datos de C-2 /
-- RNF-10): cuentas, dispositivos, tokens y códigos de pairing. Nada más.
-- El tráfico C-3 transita en memoria y jamás se persiste aquí.

CREATE TABLE accounts (
    id            TEXT PRIMARY KEY,             -- ULID
    email         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,                -- Argon2id (PHC string)
    created_at    TEXT NOT NULL
);

CREATE TABLE devices (
    id           TEXT PRIMARY KEY,              -- ULID
    account_id   TEXT NOT NULL REFERENCES accounts(id),
    name         TEXT NOT NULL DEFAULT '',
    kind         TEXT NOT NULL CHECK (kind IN ('client', 'daemon')),
    created_at   TEXT NOT NULL,
    revoked_at   TEXT,
    last_seen_at TEXT
);

CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,                -- ULID
    device_id  TEXT NOT NULL REFERENCES devices(id),
    -- sha256 hex del token; el token en claro solo existe en la respuesta
    -- que lo emitió (RNF-07: opacos, rotables, revocables por dispositivo).
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);

CREATE TABLE pairing_codes (
    code          TEXT PRIMARY KEY,             -- XXX-XXX-XXX
    account_id    TEXT NOT NULL REFERENCES accounts(id),
    daemon_pubkey TEXT NOT NULL,                -- base64(Ed25519 pubkey)
    expires_at    TEXT NOT NULL,                -- TTL 5 minutos
    used_at       TEXT,
    attempts      INTEGER NOT NULL DEFAULT 0,   -- 5 fallos → 429
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_devices_account ON devices(account_id);
CREATE INDEX idx_tokens_device ON tokens(device_id);
