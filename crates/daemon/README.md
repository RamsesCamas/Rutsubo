# rutsubo-daemon

El cerebro de Rutsubo (ADR-001): API RESTful del contrato C-1, WebSocket de
eventos (C-3), agent loop con compuerta de permisos y adapter LLM (C-4).

```bash
just dev                 # arranca en 127.0.0.1:7431 (bind exclusivo loopback, RNF-04)
cat ~/.local/share/rutsubo/token   # Bearer para toda ruta salvo /v1/health
just test                # unit + integración + E2E sin red
just prepare             # regenera la caché offline de sqlx tras cambiar consultas
```

- `api/` — los 12 endpoints C-1; sobre de error único; CORS mínimo; headers
  de seguridad; sin stack traces (listo para ZAP).
- `ws.rs` — `/v1/ws`: replay ≤1000 + empalme sin duplicar seq; comandos por el
  mismo código interno que REST; ping/pong 30/90 s.
- `agent/` — loop RF-06 (tope 20 iteraciones); el rechazo de una aprobación no
  aborta el turno.
- `gate.rs` — suspensión por sesión (RF-16); la primera decisión gana (RF-17).
- `tools/` — las 5 herramientas (RF-07…RF-11); toda ruta pasa por
  `rutsubo_core::paths::resolve_within` (RNF-05), también `search`.
- `llm/` — trait C-4 + `MockProvider` determinista + `FallbackAdapter` con la
  máquina normativa (OOM, TTFT, ventana de fallos, breaker, cooldown).
  Enchufar vLLM/Ollama/API externa = implementar `LlmProvider` (RNF-18).
- `store/` — SQLite WAL; `last_seq` se incrementa en la misma transacción que
  inserta el evento: `seq` sin huecos por construcción (C-3).

Colección ejecutable de la API: `docs/api/requests.http`.
Variables de entorno: `RUTSUBO_DATA_DIR`, `RUTSUBO_BIND` (solo loopback),
`RUTSUBO_MAX_ITERATIONS`, `RUTSUBO_SPA_ORIGIN`, `RUTSUBO_EXTERNAL_API_KEY`.
