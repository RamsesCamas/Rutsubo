//! Verificación del ID token de Google (C-2 enmendado). El cliente obtiene el
//! id_token con Google Identity Services / OAuth loopback y lo canjea en el
//! relay por un `device_token`. Algoritmo portado de
//! `Rutsubo-Webapp/api/_auth.ts::verifyGoogleIdToken`:
//! firma RS256 contra las JWKS de Google, `iss` de Google, `exp`,
//! `aud ∈ client_ids`, `email_verified == true`. Devuelve `{sub, email}`.
//!
//! `DevVerifier` (RELAY_GOOGLE_DEV=1) acepta `dev:{sub}:{email}` sin red — para
//! tests y para el E2E del autor antes de tener credenciales reales.

use crate::error::RelayError;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use std::sync::RwLock;

const GOOGLE_CERTS: &str = "https://www.googleapis.com/oauth2/v3/certs";
const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// Claims que el relay necesita del id_token.
pub struct GoogleClaims {
    pub sub: String,
    pub email: String,
}

#[derive(Deserialize)]
struct RawClaims {
    sub: String,
    email: String,
    #[serde(default)]
    email_verified: bool,
}

/// Real (JWKS de Google) o Dev (token sintético en desarrollo).
pub enum Verifier {
    Real(RealVerifier),
    Dev,
}

impl Verifier {
    /// `RELAY_GOOGLE_DEV=1` → Dev; si no, Real.
    pub fn from_config(dev: bool) -> Self {
        if dev {
            tracing::warn!("relay en modo GOOGLE_DEV: acepta id_tokens de prueba, NO usar en red pública");
            Verifier::Dev
        } else {
            Verifier::Real(RealVerifier::new())
        }
    }

    pub async fn verify(
        &self,
        id_token: &str,
        client_ids: &[String],
    ) -> Result<GoogleClaims, RelayError> {
        match self {
            Verifier::Dev => verify_dev(id_token),
            Verifier::Real(v) => v.verify(id_token, client_ids).await,
        }
    }
}

/// Token de prueba: `dev:{sub}:{email}`.
fn verify_dev(id_token: &str) -> Result<GoogleClaims, RelayError> {
    let parts: Vec<&str> = id_token.splitn(3, ':').collect();
    match parts.as_slice() {
        ["dev", sub, email] if !sub.is_empty() && email.contains('@') => Ok(GoogleClaims {
            sub: (*sub).to_owned(),
            email: email.to_lowercase(),
        }),
        _ => Err(RelayError::unauthorized()),
    }
}

/// Verificador real con caché de JWKS (refresca ante un `kid` desconocido).
pub struct RealVerifier {
    http: reqwest::Client,
    jwks: RwLock<Option<JwkSet>>,
}

impl RealVerifier {
    fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            jwks: RwLock::new(None),
        }
    }

    async fn verify(
        &self,
        id_token: &str,
        client_ids: &[String],
    ) -> Result<GoogleClaims, RelayError> {
        let header = decode_header(id_token).map_err(|_| RelayError::unauthorized())?;
        let kid = header.kid.ok_or_else(RelayError::unauthorized)?;

        // Busca el kid en la caché; si falta, refresca una vez.
        let key = match self.key_for(&kid) {
            Some(k) => k,
            None => {
                self.refresh().await?;
                self.key_for(&kid).ok_or_else(RelayError::unauthorized)?
            }
        };

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(client_ids);
        validation.set_issuer(&GOOGLE_ISSUERS);
        // `exp` se valida por defecto.
        let data = decode::<RawClaims>(id_token, &key, &validation).map_err(|e| {
            use jsonwebtoken::errors::ErrorKind;
            match e.kind() {
                ErrorKind::InvalidAudience => RelayError::forbidden("audience_mismatch"),
                _ => RelayError::unauthorized(),
            }
        })?;
        if !data.claims.email_verified {
            return Err(RelayError::unauthorized());
        }
        Ok(GoogleClaims {
            sub: data.claims.sub,
            email: data.claims.email.to_lowercase(),
        })
    }

    fn key_for(&self, kid: &str) -> Option<DecodingKey> {
        let guard = self.jwks.read().unwrap();
        let jwk = guard.as_ref()?.find(kid)?;
        DecodingKey::from_jwk(jwk).ok()
    }

    async fn refresh(&self) -> Result<(), RelayError> {
        let set: JwkSet = self
            .http
            .get(GOOGLE_CERTS)
            .send()
            .await
            .map_err(RelayError::internal)?
            .json()
            .await
            .map_err(RelayError::internal)?;
        *self.jwks.write().unwrap() = Some(set);
        Ok(())
    }
}
