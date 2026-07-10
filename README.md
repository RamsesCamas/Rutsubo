# Rutsubo

**Tu agente de código. Tu GPU. Tu workspace.**

Rutsubo es un agente de código *local-first*: un daemon que reside en el equipo del
desarrollador, ejecuta un modelo de lenguaje compacto sobre hardware de consumidor y
expone su estado a las superficies cliente sin exponer jamás puertos entrantes a redes
públicas.

Este repositorio contiene el **backend** completo:

| Crate | Descripción |
|---|---|
| `crates/core` (`rutsubo-core`) | Contrato único del protocolo: sobre de eventos (C-3), catálogo de eventos y comandos, validación de rutas anti-traversal, representación de diffs. Los tipos TypeScript del cliente web se generan desde aquí con `ts-rs`. |
| `crates/daemon` (`rutsubo-daemon`) | API RESTful (contrato C-1) sobre `127.0.0.1:7431`, WebSocket local de eventos (C-3), agent loop con 5 herramientas, permission gate y adapter LLM con fallback (C-4). |

La interfaz web vive en su propio repositorio:
[Rutsubo-Webapp](https://github.com/RamsesCamas/Rutsubo-Webapp).

## Requisitos

- Rust 1.92+ (edition 2024)
- [`just`](https://github.com/casey/just) para las tareas de desarrollo
- `sqlx-cli` para regenerar la caché offline de consultas (`cargo install sqlx-cli --no-default-features --features rustls,sqlite`)

## Uso rápido

```bash
just dev        # compila y arranca el daemon en 127.0.0.1:7431
just test       # cargo test --workspace
just bindings   # regenera crates/core/bindings (falla si hay drift sin commitear)
just lint       # fmt + clippy
```

En el primer arranque el daemon genera un token local en
`~/.local/share/rutsubo/token` (permisos `0600`). Toda petición salvo `GET /v1/health`
exige `Authorization: Bearer <token>`.

## Documentación

- `docs/api/requests.http` — colección ejecutable de los 12 endpoints del contrato C-1.
- `docs/decisions/` — decisiones de implementación y su trazabilidad con los ADRs.

Proyecto académico — Maestría en Ciencias e Innovación Tecnológica (UPCh),
materia *Tecnologías para el Desarrollo de Aplicaciones*.
