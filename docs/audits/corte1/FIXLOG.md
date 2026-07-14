# FIXLOG — Corte 1 de métricas y seguridad

Sesión: 2026-07-13 (Gidorah, WSL2). Objetivo: Lighthouse + OWASP ZAP con
evidencia reproducible → `RamsesCamas_Rutsubo_ReporteMetricas.pdf`.
Regla Cero: ninguna cifra inventada; cada número sale de un JSON crudo aquí
guardado con su SHA-256 en el anexo del PDF. Cada entrada: síntoma → causa → fix.

## 1. Docker apagado a mitad de sesión
- **Síntoma**: `docker run` falla ("could not be found in this WSL 2 distro").
- **Causa**: la integración WSL de Docker Desktop se desactiva sola entre
  sesiones (recurrente). El CLI symlink existe, pero el motor no responde.
- **Fix**: reactivar Docker Desktop en Windows (paso del autor). Confirmado con
  `docker run --rm hello-world`.

## 2. ZAP en contenedor no alcanza el host por `--network=host`
- **Síntoma**: `zap-baseline -t http://127.0.0.1:7431` con `--network=host` →
  todo 404/timeout.
- **Causa**: en Docker Desktop + WSL2, `--network=host` usa la red de la VM
  `docker-desktop`, no la del distro donde corren daemon/relay.
- **Fix**: apuntar a `http://host.docker.internal:PUERTO` (forwarding
  localhost WSL↔Windows). Es el mismo daemon/relay de `127.0.0.1` (respuesta
  idéntica). Documentado en la metodología del reporte.

## 3. El Bearer del daemon rompía el escaneo autenticado
- **Síntoma**: el baseline del daemon aborta con "File not found '<token>'".
- **Causa**: `zap-baseline -z "…replacement=Bearer <token>"` separa el `-z` por
  espacios; el espacio de "Bearer <token>" partía la config y ZAP tomaba el
  token como nombre de archivo.
- **Fix (mejor método)**: en vez del `spider` clásico —inútil en una API JSON
  sin raíz navegable— se levantó ZAP en modo daemon y se condujeron los
  endpoints reales por su **proxy** con `curl -H "Authorization: Bearer …"`,
  auditando pasivamente las respuestas efectivas (200 autenticadas, 401, 404).
  Cobertura confirmada vía la API de ZAP: 11 URLs del daemon.

## 4. LaTeX: `spanish` de babel ausente
- **Causa**: sin `texlive-lang-spanish` (no hay sudo).
- **Fix**: quitar babel; `\emergencystretch` + `\sloppy` + `\hyphenation`
  manuales evitan overfull sin patrones de guionado.

## 5. LaTeX: "Misplaced \noalign" con `\input` dentro de tablas
- **Causa**: `\input` de un fragmento de filas dentro de `tabular`/`tabularx`
  deja una fila fantasma antes de `\bottomrule`.
- **Fix**: incrustar las filas (generadas desde los JSON, sin transcripción a
  mano) directamente en la tabla; `tabularx` solo para columnas de texto largo.

## 6. Higiene de secretos en la evidencia
- **Síntoma**: `zap/auth.prop` quedó con `Bearer <token>` del daemon de prueba.
- **Fix**: eliminado antes de commitear; escaneo de todo `corte1/` confirma cero
  secretos (token efímero, `gsk_`, `GOCSPX-`). Las instancias auditadas fueron
  desechables (data-dir temporal + workspace dummy), nunca trabajo real.

## Resultado
- Lighthouse (Inicio/Login × móvil/escritorio, mediana de 3): Perf/A11y/SEO 100,
  Best Practices 96 (404 de `favicon.ico`), FCP 165–620 ms < 1500 (RNF-02).
- ZAP: daemon 0 hallazgos (11 endpoints autenticados); relay 1 Medio (CORS
  permisivo) + 1 Bajo (falta `nosniff`). 0 Alto, 0 FAIL.
- Corrección en caliente (4.3) no necesaria: errores = `ErrorEnvelope`, sin fugas.
