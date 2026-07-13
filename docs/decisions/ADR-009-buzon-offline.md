# ADR-009 — Buzón de tareas offline e identidad unificada con Google (C-2)

- **Estado:** Aceptado — **implementación parcial (M2)**. M3 (cifrado
  extremo-a-extremo del buzón) queda **pendiente obligatorio** antes de
  cualquier despliegue fuera de LAN.
- **Fecha:** 2026-07
- **Ámbito:** `crates/relay`, `crates/daemon`, `crates/core` (contrato C-4) y las
  tres superficies (móvil, web, escritorio).
- **Jerarquía:** contrato C-n > ADR-n > este documento.

## Contexto

C-2 preveía cuentas del relay con correo+contraseña (Argon2id) y el pairing del
daemon. Dos necesidades del producto lo enmiendan:

1. **Identidad unificada.** El usuario quiere ver las mismas sesiones desde el
   móvil, la web y el escritorio sin gestionar credenciales distintas por
   superficie. Google Sign-In centraliza la cuenta.
2. **Lanzar trabajo con el escritorio apagado.** Desde el móvil se debe poder
   *encolar* una tarea que se ejecute cuando el daemon del escritorio vuelva a
   estar en línea, y recibir la aprobación en el teléfono.

La arquitectura pub/sub del relay (ADR-006) ya permite (1) —verificado E2E—;
Google solo unifica la cuenta. (2) es un **buzón store-and-forward**, que choca
con RNF-10 ("el relay no persiste tránsito") y por tanto necesita una excepción
explícita y acotada.

## Decisión

### M1 — Google Sign-In

- El cliente obtiene un `id_token` de Google (GIS en web, `google_sign_in`
  nativo en móvil, OAuth loopback con PKCE en escritorio) y lo canjea en
  `POST /v1/auth/google` por un `device_token` opaco del relay. La cuenta se
  llavea por el `sub` de Google; el relay valida JWKS RS256, `aud`, `iss`,
  `exp` y `email_verified`. Un modo `RELAY_GOOGLE_DEV=1` acepta `dev:sub:correo`
  para pruebas herméticas (jamás en red pública).
- Se **elimina** `register`/`token` (contraseña). Se conservan `issue_token`,
  `authenticate`, `require_bearer`, `rotate` y el pairing con firma Ed25519.
- El transporte del **escritorio sigue siendo local** (rápido, RNF-11); Google
  solo unifica la cuenta y **parea el daemon** a ella para que sus sesiones
  locales se difundan al móvil y la web.

### M2 — Buzón de tareas offline

- El buzón vive en el **relay** (tabla `outbox` + REST `/v1/outbox`). Es la
  **única excepción** a RNF-10: el relay persiste *contenido de tareas*.
- Solo se encolan **mensajes** (`send_message` diferido); nunca aprobaciones ni
  control (ADR-007, RF-14). Una tarea encolada se entrega como
  `ToDaemon{outbox_id, frame: CommandEnvelope::SendMessage, new_session_title?}`,
  reusando todo el pipeline `send_message_inner`.
- **Entrega:** al conectar el daemon se **drena** el buzón FIFO; con el daemon ya
  conectado la entrega es inmediata. **At-least-once** con **dedup del lado del
  daemon** (`outbox_acks`, `INSERT OR IGNORE` por `outbox_id`): un reinicio a
  mitad de drenaje no duplica. El daemon acusa con `FromDaemon{ack_outbox_id}` y
  el relay borra la fila.
- **Límites (anti-abuso, no negociables):** payload ≤ 32 KB, ≤ 20 tareas
  encoladas por cuenta, TTL 7 días (config `RELAY_OUTBOX_TTL_SECS`).
  Idempotencia por `(account_id, client_msg_id)`.
- **Presencia honesta:** el relay avisa `daemon_unavailable` a un suscriptor al
  conectar sin daemon, y por difusión cuando el daemon se cae, para que la UI
  muestre "escritorio offline"/"Encolar" sin reintentar un comando.
- **Contrato C-4:** tipos `Outbox*` (`api.rs`) y evento `task_dequeued`
  (`events.rs`); campos opcionales `outbox_id`/`new_session_title`/
  `ack_outbox_id` en `ToDaemon`/`FromDaemon` (internos Rust↔Rust en `relay.rs`,
  retrocompatibles, no tocan la versión de contrato salvo el bump 3→4 por los
  tipos y el evento nuevos).

## Consecuencias

- **Seguridad (restricción dura).** En M2 el payload del buzón viaja y se
  guarda **en claro** en el relay. Por tanto el relay **no sale de LAN** hasta
  M3: bind loopback/LAN, nunca `0.0.0.0` público. Esta es la única persistencia
  de contenido del relay y está acotada a `send_message`.
- **M3 pendiente obligatorio.** Antes de cualquier despliegue público hay que
  cifrar el payload extremo-a-extremo para el daemon (sealed box X25519,
  `payload_kind: "sealed_box"`): el relay guardaría solo ciphertext opaco,
  restaurando el espíritu de RNF-10. Hasta entonces, este ADR queda en
  "implementación parcial".
- **Prohibiciones que se mantienen:** WebView embebido para OAuth (se usa el
  navegador del sistema), buzón sin límites, aprobaciones encoladas, y bind
  público del relay.
- **Verificación.** Tests del relay (canje Google aud correcta/incorrecta y
  dev; outbox enqueue/drain/ack/dedup/límites/cancel; presencia). E2E del feature
  (móvil): encolar con el escritorio apagado → "En cola" → parear el daemon →
  `task_dequeued` limpia la cola → la aprobación llega al teléfono → aprobar →
  el daemon continúa.
