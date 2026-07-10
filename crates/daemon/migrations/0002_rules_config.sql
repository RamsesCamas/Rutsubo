-- Extensión al esquema del handoff (documentada en docs/decisions/):
-- C-1 expone GET/PUT /v1/rules y GET/PUT /v1/config/model, que necesitan
-- persistencia; el esquema 0001 no las contemplaba.

-- Reglas estables de auto-aprobación (RF-18). La evaluación de reglas en la
-- compuerta es TODO(fase-3); aquí solo persistencia y CRUD por contrato.
CREATE TABLE rules (
  id TEXT PRIMARY KEY,                -- ULID
  workspace_path TEXT NOT NULL,
  tool TEXT NOT NULL,                 -- run_shell|write_file|edit_file
  pattern TEXT NOT NULL,              -- patrón exacto (comando literal para run_shell)
  created_at TEXT NOT NULL
);

-- Configuración clave-valor del daemon. key='model' guarda la política del
-- adapter LLM (C-1 /v1/config/model) para que sobreviva reinicios.
CREATE TABLE config (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL                 -- JSON
);
