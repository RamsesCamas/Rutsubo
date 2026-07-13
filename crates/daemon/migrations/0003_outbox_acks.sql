-- Dedup local de tareas drenadas del buzón (ADR-009): el relay entrega
-- at-least-once; el daemon procesa cada outbox_id una sola vez.
CREATE TABLE outbox_acks (
    outbox_id  TEXT PRIMARY KEY,
    applied_at TEXT NOT NULL
);
