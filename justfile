# Tareas de desarrollo de Rutsubo (backend)

# Pull + push (hábito de apertura/cierre multi-máquina, plan general §1)
sync:
    git pull --rebase && git push

# Arranca el daemon en 127.0.0.1:7431
dev:
    cargo run -p rutsubo-daemon

# Tests de todo el workspace
test:
    cargo test --workspace

# Regenera los bindings TypeScript desde rutsubo-core.
# Falla si quedan cambios sin commitear (RNF-17): en CI esto detecta drift.
bindings:
    cargo test -p rutsubo-core export_bindings
    git diff --exit-code -- crates/core/bindings

# Materializa contract-export/ (VERSION, schema, fixtures, bindings-ts, CHECKSUM),
# el contrato que los repos de app vendorizan con `just sync-contract`.
# Falla si queda drift sin commitear; el guard de VERSION vive en CI.
contract-export:
    cargo test -p rutsubo-core export_bindings
    cargo test -p rutsubo-core --test contract_export
    git diff --exit-code -- contract-export

# Arranca el relay C-2 en 127.0.0.1:8443
relay:
    cargo run -p rutsubo-relay

# Regenera la caché offline de sqlx (tras cambiar consultas o migraciones).
# Requiere sqlx-cli 0.8: cargo install sqlx-cli --version "^0.8" \
#   --no-default-features --features rustls,sqlite
prepare:
    cd crates/daemon && \
      DATABASE_URL="sqlite://$PWD/crates/daemon/.sqlx-dev.db" sqlx database create && \
      DATABASE_URL="sqlite://$PWD/crates/daemon/.sqlx-dev.db" sqlx migrate run && \
      DATABASE_URL="sqlite://$PWD/crates/daemon/.sqlx-dev.db" cargo sqlx prepare

# Formato y lints
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Gate local equivalente a CI. El patrón exige la forma de un token Groq real
# (prefijo + ≥40 base62) para no auto-detectarse: el literal de esta misma
# receta (`gsk_[…`) no cumple el patrón, así que no cuenta como filtración.
check-secrets:
    ! git log -p --all | rg -q 'gsk_[A-Za-z0-9]{40}'

ci: lint test bindings contract-export check-secrets
