# rutsubo-core

Contrato único del protocolo (ADR-004): la fuente de verdad de todo lo que
viaja entre el daemon y sus clientes.

- `envelope.rs` — sobre v1 de C-3: `{v, type, payload, session_id, seq, ts}`.
- `events.rs` — catálogo exhaustivo de 11 eventos (enum *adjacently tagged*).
- `commands.rs` — 4 comandos cliente→daemon + `CommandEnvelope`.
- `ids.rs` — newtypes ULID (`SessionId`, `ApprovalId`, …) y `ProviderId`.
- `paths.rs` — `resolve_within`: validación anti-traversal (RNF-05).
- `diff.rs` — `FileDiff` unified con conteo de +/− (RF-27).
- `api.rs` — esquemas request/response y filtros del contrato C-1.

## Bindings TypeScript

Todos los tipos públicos derivan `TS` y se exportan a `bindings/` (RNF-17):

```bash
just bindings   # regenera y falla si hay drift sin commitear
```

El cliente web consume estos archivos vendorizados; **jamás** se redeclara un
tipo del protocolo a mano. Cambiar el payload de un evento existente rompe
compatibilidad: exige incrementar `v` y detenerse a consultar.

```bash
cargo test -p rutsubo-core   # unit + round-trip serde de cada evento/comando
```
