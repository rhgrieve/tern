use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reads a token's claims. The signature is not checked: the claims only steer our own cache and
/// display, and the token came over TLS from the sign-in service.
pub(crate) fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub object_id: String,
    pub tenant_id: String,
    pub name: String,
}

impl Identity {
    pub(crate) fn from_id_token(token: &str) -> Option<Identity> {
        let claims = claims(token)?;
        Some(Identity {
            object_id: claims["oid"].as_str()?.to_owned(),
            tenant_id: claims["tid"].as_str()?.to_owned(),
            name: claims["name"]
                .as_str()
                .or(claims["preferred_username"].as_str())
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

/// What a token allows; safe to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub scopes: Vec<String>,
    pub expires_at: Option<u64>,
}

impl TokenInfo {
    pub fn of(token: &str) -> TokenInfo {
        let claims = claims(token).unwrap_or(Value::Null);
        TokenInfo {
            scopes: claims["scp"]
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            expires_at: claims["exp"].as_u64(),
        }
    }
}

#[cfg(test)]
pub(crate) fn fake(claims: Value) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_identity_and_scopes() {
        let token = fake(json!({"oid": "o", "tid": "t", "name": "Alex", "scp": "a b", "exp": 5}));
        let identity = Identity::from_id_token(&token).unwrap();
        assert_eq!(
            (identity.object_id.as_str(), identity.tenant_id.as_str()),
            ("o", "t")
        );
        assert_eq!(TokenInfo::of(&token).scopes, ["a", "b"]);
        assert_eq!(
            TokenInfo::of("junk"),
            TokenInfo {
                scopes: vec![],
                expires_at: None
            }
        );
        assert!(Identity::from_id_token("a.b.c").is_none());
    }
}
