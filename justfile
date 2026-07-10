# Tareas de desarrollo de Rutsubo (backend)

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

# Gate local equivalente a CI
check-secrets:
    ! git log -p --all | rg -q 'gsk_'

ci: lint test bindings check-secrets
