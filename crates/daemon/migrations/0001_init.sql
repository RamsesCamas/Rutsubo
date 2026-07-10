-- Esquema base del daemon (handoff §4.3, literal).
-- WAL se activa al abrir la conexión; last_seq se incrementa en la misma
-- transacción que inserta el evento: esa atomicidad garantiza seq sin huecos (C-3).

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,                -- ULID
  workspace_path TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'idle', -- idle|running|waiting_approval|archived
  created_at TEXT NOT NULL,
  last_seq INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE events (
  session_id TEXT NOT NULL REFERENCES sessions(id),
  seq INTEGER NOT NULL,
  type TEXT NOT NULL,
  payload TEXT NOT NULL,              -- JSON del evento completo (sobre C-3)
  ts TEXT NOT NULL,
  PRIMARY KEY (session_id, seq)
);

CREATE TABLE messages (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  role TEXT NOT NULL,                 -- user|assistant
  content TEXT NOT NULL,
  client_msg_id TEXT,                 -- idempotencia
  created_at TEXT NOT NULL,
  UNIQUE (session_id, client_msg_id)
);

CREATE TABLE approvals (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  tool_call_id TEXT NOT NULL,
  tool TEXT NOT NULL,
  summary TEXT NOT NULL,
  args TEXT NOT NULL,                 -- JSON
  decision TEXT,                      -- NULL=pendiente | approve|reject
  resolved_by TEXT,
  created_at TEXT NOT NULL,
  resolved_at TEXT
);

CREATE TABLE audit_log (
  id TEXT PRIMARY KEY,
  session_id TEXT,
  kind TEXT NOT NULL,                 -- tool_exec|llm_call|approval|config
  detail TEXT NOT NULL,               -- JSON (para llm_call: provider_id, RF-22)
  ts TEXT NOT NULL
);
