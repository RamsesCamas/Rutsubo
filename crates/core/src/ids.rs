//! Identificadores del sistema: newtypes sobre ULID (convención transversal
//! del contrato: ordenables lexicográficamente por tiempo de creación).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ts_rs::TS;
use ulid::Ulid;

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS,
            schemars::JsonSchema,
        )]
        #[ts(export)]
        pub struct $name(
            #[ts(type = "string")]
            #[schemars(with = "String")]
            pub Ulid,
        );

        impl $name {
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Ulid::new())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Ulid::from_str(s)?))
            }
        }
    };
}

ulid_id!(
    /// Identificador de una sesión de conversación (RF-01).
    SessionId
);
ulid_id!(
    /// Identificador de un mensaje (usuario o asistente).
    MessageId
);
ulid_id!(
    /// Identificador de una solicitud de aprobación (RF-14).
    ApprovalId
);
ulid_id!(
    /// Identificador de una invocación de herramienta.
    ToolCallId
);
ulid_id!(
    /// Identificador de una entrada del audit log (RF-05).
    AuditId
);
ulid_id!(
    /// Identificador de una regla de auto-aprobación (RF-18).
    RuleId
);

/// Identificador estable de un proveedor de modelo para el audit log (RF-22).
/// No es un ULID: es un nombre jerárquico legible, p. ej. `local:vllm:qwen3.5-8b`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ProviderId(pub String);

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}
