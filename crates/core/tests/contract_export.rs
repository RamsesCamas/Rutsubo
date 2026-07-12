//! Materializa `contract-export/` en la raíz del repo: el contrato versionado
//! que los repos de app (Rutsubo-DesktopApp, Rutsubo-Mobile-App) vendorizan
//! como `contract/` vía `just sync-contract`.
//!
//! Se invoca por nombre desde `just contract-export` (mismo patrón que los
//! tests `export_bindings_*` de ts-rs). La receta termina con
//! `git diff --exit-code -- contract-export`: cualquier drift falla en CI.
//!
//! Estructura generada:
//!   contract-export/
//!   ├── VERSION            ← rutsubo_core::CONTRACT_VERSION (bump manual)
//!   ├── CHECKSUM           ← sha256 del contenido (detecta bump olvidado en CI)
//!   ├── schema/            ← JSON Schema de los dos sobres (schemars)
//!   ├── fixtures/          ← un JSON por fixture (event/ y command/)
//!   └── bindings-ts/       ← copia de crates/core/bindings (ts-rs)

use rutsubo_core::CONTRACT_VERSION;
use rutsubo_core::commands::CommandEnvelope;
use rutsubo_core::envelope::Envelope;
use rutsubo_core::events::Event;
use rutsubo_core::fixtures::{command_fixtures, event_fixtures};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn export_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/core → la raíz del repo está dos niveles arriba.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("raíz del repo")
        .join("contract-export")
}

fn write_pretty(path: &Path, value: &serde_json::Value) {
    let mut body = serde_json::to_string_pretty(value).expect("serializar JSON");
    body.push('\n');
    fs::write(path, body).unwrap_or_else(|e| panic!("escribir {}: {e}", path.display()));
}

/// Recorre un directorio en orden lexicográfico estable y devuelve
/// (ruta relativa, contenido) de cada archivo.
fn collect_files(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("leer {}: {e}", dir.display()))
        .map(|e| e.expect("entrada").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.push((rel, fs::read(&path).expect("leer archivo")));
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("crear destino");
    for entry in fs::read_dir(src).expect("leer origen") {
        let path = entry.expect("entrada").path();
        let target = dst.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).expect("copiar archivo");
        }
    }
}

#[test]
fn export_contract() {
    let root = export_root();

    // Limpieza: los directorios generados se reconstruyen desde cero para que
    // un fixture renombrado no deje archivos huérfanos.
    for sub in ["schema", "fixtures", "bindings-ts"] {
        let dir = root.join(sub);
        if dir.exists() {
            fs::remove_dir_all(&dir).expect("limpiar directorio generado");
        }
    }
    fs::create_dir_all(root.join("schema")).unwrap();
    fs::create_dir_all(root.join("fixtures/event")).unwrap();
    fs::create_dir_all(root.join("fixtures/command")).unwrap();

    // VERSION
    fs::write(root.join("VERSION"), format!("{CONTRACT_VERSION}\n")).unwrap();

    // schema/ — un JSON Schema por sobre (oneOf por variante, ver schema_shape).
    let event_schema = schemars::schema_for!(Envelope<Event>);
    write_pretty(
        &root.join("schema/event_envelope.schema.json"),
        &serde_json::to_value(&event_schema).unwrap(),
    );
    let command_schema = schemars::schema_for!(CommandEnvelope);
    write_pretty(
        &root.join("schema/command_envelope.schema.json"),
        &serde_json::to_value(&command_schema).unwrap(),
    );

    // fixtures/ — un archivo por fixture del catálogo.
    for (name, fixture) in event_fixtures() {
        write_pretty(&root.join(format!("fixtures/event/{name}.json")), &fixture);
    }
    for (name, fixture) in command_fixtures() {
        write_pretty(
            &root.join(format!("fixtures/command/{name}.json")),
            &fixture,
        );
    }

    // bindings-ts/ — copia literal de los bindings ts-rs ya exportados.
    // `just contract-export` corre export_bindings antes que este test.
    let bindings_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings");
    assert!(
        bindings_src.exists(),
        "crates/core/bindings no existe: corre `cargo test -p rutsubo-core export_bindings` primero"
    );
    copy_dir(&bindings_src, &root.join("bindings-ts"));

    // CHECKSUM — sha256 de (ruta relativa + contenido) de todo lo generado,
    // en orden estable. CI compara contra origin/main: si cambió sin bump de
    // VERSION, falla.
    let mut files = Vec::new();
    for sub in ["schema", "fixtures", "bindings-ts"] {
        collect_files(&root, &root.join(sub), &mut files);
    }
    let mut hasher = Sha256::new();
    for (rel, content) in &files {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(content);
    }
    let digest = hasher.finalize();
    fs::write(root.join("CHECKSUM"), format!("{digest:x}\n")).unwrap();
}

/// Congela la forma del schema que emite schemars para el enum adjacently
/// tagged: si un upgrade de schemars cambia la representación, falla aquí (y
/// en el drift de `just contract-export`), no en los consumidores Dart/TS.
#[test]
fn schema_shape() {
    let schema = serde_json::to_value(schemars::schema_for!(Envelope<Event>)).unwrap();

    // El sobre aplana el enum con `#[serde(flatten)]`: las variantes viven en
    // un combinador (anyOf/oneOf) en la raíz del schema.
    let variants = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("el schema debe tener oneOf/anyOf en la raíz: {schema:#}"));
    assert_eq!(variants.len(), 11, "11 eventos C-3");

    let expected_kinds = [
        "session_state",
        "message_delta",
        "message_completed",
        "tool_call_requested",
        "approval_request",
        "approval_resolved",
        "tool_result",
        "file_diff",
        "model_provider_changed",
        "daemon_unavailable",
        "error",
    ];
    let kinds: Vec<&str> = variants
        .iter()
        .map(|v| {
            v.pointer("/properties/type/const")
                .and_then(|c| c.as_str())
                .unwrap_or_else(|| panic!("cada variante debe fijar type con const: {v:#}"))
        })
        .collect();
    assert_eq!(
        kinds, expected_kinds,
        "discriminantes en orden de declaración"
    );

    // daemon_unavailable es variante unit: exige type pero no payload.
    let unavailable = &variants[9];
    let required: Vec<&str> = unavailable
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(required.contains(&"type"));
    assert!(
        !required.contains(&"payload"),
        "la variante unit no debe exigir payload"
    );

    // Campos del sobre: v y ts requeridos; seq opcional (None en comandos).
    let props = schema.get("properties").expect("properties del sobre");
    for field in ["v", "session_id", "seq", "ts"] {
        assert!(
            props.get(field).is_some(),
            "el sobre debe declarar `{field}`"
        );
    }
    let root_required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(root_required.contains(&"v"));
    assert!(root_required.contains(&"ts"));
    assert!(
        !root_required.contains(&"seq"),
        "seq no puede ser requerido: los comandos no lo llevan"
    );
}
