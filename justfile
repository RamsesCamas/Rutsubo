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

# Formato y lints
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Gate local equivalente a CI
ci: lint test bindings
