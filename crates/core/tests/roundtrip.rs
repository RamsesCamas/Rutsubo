//! Round-trip serde para cada evento y comando del catálogo C-3:
//! JSON (fixture con la forma exacta del contrato) → tipo → JSON idéntico.
//!
//! Los fixtures viven en `rutsubo_core::fixtures` (fuente única, compartida
//! con `contract_export.rs` que los materializa para los repos de app).

use rutsubo_core::commands::CommandEnvelope;
use rutsubo_core::envelope::Envelope;
use rutsubo_core::events::Event;
use rutsubo_core::fixtures::{command_fixtures, event_fixtures};
use serde_json::json;

#[test]
fn todos_los_eventos_hacen_roundtrip_identico() {
    for (name, fixture) in event_fixtures() {
        let parsed: Envelope<Event> = serde_json::from_value(fixture.clone())
            .unwrap_or_else(|e| panic!("el fixture `{name}` debe deserializar: {e}"));
        let back = serde_json::to_value(&parsed)
            .unwrap_or_else(|e| panic!("el tipo de `{name}` debe serializar: {e}"));
        assert_eq!(
            fixture, back,
            "`{name}`: el round-trip debe ser idéntico campo a campo"
        );
    }
}

#[test]
fn todos_los_comandos_hacen_roundtrip_identico() {
    for (name, fixture) in command_fixtures() {
        let parsed: CommandEnvelope = serde_json::from_value(fixture.clone())
            .unwrap_or_else(|e| panic!("el fixture `{name}` debe deserializar: {e}"));
        let back = serde_json::to_value(&parsed)
            .unwrap_or_else(|e| panic!("el tipo de `{name}` debe serializar: {e}"));
        assert_eq!(
            fixture, back,
            "`{name}`: el round-trip debe ser idéntico campo a campo"
        );
    }
}

#[test]
fn el_catalogo_cubre_todos_los_eventos_y_comandos() {
    // Si se añade una variante al enum sin fixture, este test lo denuncia:
    // el contrato exportado quedaría incompleto para los repos de app.
    let event_kinds: std::collections::BTreeSet<String> = event_fixtures()
        .into_iter()
        .map(|(_, f)| f["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        event_kinds.len(),
        12,
        "cada uno de los 12 eventos C-3 debe tener al menos un fixture"
    );
    let command_kinds: std::collections::BTreeSet<String> = command_fixtures()
        .into_iter()
        .map(|(_, f)| f["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        command_kinds.len(),
        4,
        "cada uno de los 4 comandos C-3 debe tener al menos un fixture"
    );
}

#[test]
fn el_sobre_de_evento_expone_seq_y_version() {
    let env: Envelope<Event> = serde_json::from_value(json!({
        "v": 1, "type": "message_delta",
        "payload": {"message_id": "01J1ZH2K0000000000000000AA", "delta": "x"},
        "session_id": rutsubo_core::fixtures::SID, "seq": 9, "ts": rutsubo_core::fixtures::TS
    }))
    .unwrap();
    assert_eq!(env.v, 1);
    assert_eq!(env.seq, Some(9));
    assert_eq!(env.body.kind(), "message_delta");
}
