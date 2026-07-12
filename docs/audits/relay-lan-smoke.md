# Smoke test del relay C-2 en LAN

Procedimiento reproducible para validar el relay (ADR-006 / contrato C-2) sin
despliegue: primero localhost con `cargo run`, luego LAN para el iPhone físico,
y por último la variante docker. El objetivo de la fase es dejar el despliegue
reducido a un `railway up`.

Estado: **verificado en localhost con `cargo run` y en docker compose**
(2026-07-11). La variante LAN física (iPhone) queda como paso operativo del
autor en el MacBook (ver notas).

---

## 1. Localhost con `cargo run` (verificado)

Tres terminales en `~/MaestriaCyT/TPCA/Rutsubo` (o `~/dev/Rutsubo` en el Mac):

```bash
# T1 — relay
just relay                         # 127.0.0.1:8443

# T2 — daemon apuntando al relay
RUTSUBO_RELAY_URL=http://127.0.0.1:8443 just dev     # 127.0.0.1:7431

# T3 — pairing por curl
DTOKEN=$(cat ~/.local/share/rutsubo/token)           # o $RUTSUBO_DATA_DIR/token
curl -s -X POST localhost:8443/v1/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"tu@correo","password":"secreta-123"}'
TOK=$(curl -s -X POST localhost:8443/v1/auth/token \
  -H 'content-type: application/json' \
  -d '{"email":"tu@correo","password":"secreta-123","device_name":"cli"}' | jq -r .token)

# La app de escritorio (autenticada en el relay) lee la pubkey del daemon…
PUB=$(curl -s -H "Authorization: Bearer $DTOKEN" localhost:7431/v1/relay/status | jq -r .pubkey_b64)
# …crea el código de pairing…
CODE=$(curl -s -X POST -H "Authorization: Bearer $TOK" \
  -H 'content-type: application/json' localhost:8443/v1/pairing/codes \
  -d "{\"daemon_pubkey\":\"$PUB\"}" | jq -r .code)
# …y el daemon lo reclama firmándolo.
curl -s -X POST -H "Authorization: Bearer $DTOKEN" \
  -H 'content-type: application/json' localhost:7431/v1/relay/pair -d "{\"code\":\"$CODE\"}"

# Verificación:
curl -s localhost:7431/v1/health | jq .relay      # {configured:true, connected:true}
```

Un suscriptor (`ws://127.0.0.1:8443/v1/subscribe?token=$TOK`) recibe los eventos
del daemon difundidos por el relay; enviar un comando sin daemon conectado
responde `daemon_unavailable` (sin `seq`, no encolado). Un segundo daemon de la
misma cuenta desplaza al primero con **close 4001** (`superseded`); una conexión
sin pong en 90 s cierra con **4002** (`idle`).

**Resultado esperado** (todos verificados con los tests de `crates/relay/tests`
y el smoke manual):

| Comprobación | Esperado |
|---|---|
| `health.relay` tras pairing | `{configured:true, connected:true}` |
| evento del daemon → suscriptor | llega difundido, `seq` intacto |
| backlog `subscribe_session` | unicast al dispositivo, dedup por `seq` |
| comando sin daemon | `daemon_unavailable`, no encolado |
| segundo daemon | 4001 `superseded` al primero |
| idle > 90 s | 4002 `idle` |

---

## 2. LAN para el iPhone físico

El teléfono no ve el localhost del host. Se levanta el relay accesible en la LAN
y tanto el daemon como el iPhone conectan a la IP del host.

```bash
# Relay escuchando en todas las interfaces:
RELAY_BIND=0.0.0.0:8443 cargo run -p rutsubo-relay
# Daemon en el mismo host, apuntando al relay por su IP de LAN:
RUTSUBO_RELAY_URL=http://<ip-del-host>:8443 just dev
```

En la app iOS: Ajustes → transporte **Relay** → `ws://<ip-del-host>:8443` →
login (email/password) → Conectar. La `NSLocalNetworkUsageDescription` del
`Info.plist` es obligatoria en iOS 14+ para hablar con IPs de la LAN.

**WSL2 (host Gidorah):** para exponer el puerto del relay a la LAN física puede
hacer falta habilitar *mirrored networking* (`.wslconfig`: `networkingMode=mirrored`)
o un `netsh interface portproxy` en Windows que reenvíe `8443` a la IP de WSL.
El MacBook, al ser host nativo, no necesita esto.

**TLS:** en LAN se usa `ws://` plano (deliberado): `dart:io` de Flutter no pasa
por el ATS de iOS, así que el iPhone habla `ws://` en LAN sin certificados. El
despliegue público (Railway) termina TLS y entrega `wss://` gratis — el mismo
código de transporte, solo cambia la URL.

---

## 3. Variante docker (verificada)

```bash
docker compose up -d relay          # publica 8443, volumen sqlite persistente
curl -s localhost:8443/v1/health    # {"status":"ok","version":"0.1.0"}
```

Verificado (2026-07-11): la imagen `rutsubo-relay` construye con
`docker compose build relay`; con el contenedor arriba, el pairing de la
sección 1 apuntando a `localhost:8443` completa y `health.relay.connected` pasa
a `true` (daemon del host ↔ relay en contenedor). El volumen `relay-data`
persiste cuentas/dispositivos entre reinicios.

Con esto, **el despliegue queda reducido a `railway up`**: mismo binario, misma
imagen; solo cambia dónde corre y que Railway termina TLS (`wss://`).
