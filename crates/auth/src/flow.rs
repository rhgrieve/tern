//! The authorization-code flow with PKCE. The sign-in window shows the URL and catches the redirect.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{CLIENT_ID, Error, Result, SPACES};

pub const REDIRECT: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";
pub(crate) const AUTHORITY: &str = "https://login.microsoftonline.com";

fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn scope(resource: &str) -> String {
    format!("{resource}/.default openid profile offline_access")
}

/// What the sign-in window needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignInRequest {
    pub url: String,
    pub redirect: String,
}

/// One attempt to sign in. Holds the PKCE verifier and state, which never leave this process.
pub struct SignIn {
    verifier: String,
    state: String,
}

impl Default for SignIn {
    fn default() -> Self {
        SignIn {
            verifier: random(),
            state: random(),
        }
    }
}

impl SignIn {
    pub fn new() -> Self {
        Self::default()
    }

    /// `claims` answers a claims challenge from an earlier [`Error::InteractionRequired`].
    pub fn request(&self, claims: Option<&str>) -> SignInRequest {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        let mut url = Url::parse(&format!("{AUTHORITY}/organizations/oauth2/v2.0/authorize"))
            .expect("constant url");
        url.query_pairs_mut()
            .append_pair("client_id", CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("response_mode", "query")
            .append_pair("redirect_uri", REDIRECT)
            .append_pair("scope", &scope(SPACES))
            .append_pair("state", &self.state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        if let Some(claims) = claims {
            url.query_pairs_mut().append_pair("claims", claims);
        }
        SignInRequest {
            url: url.into(),
            redirect: REDIRECT.to_owned(),
        }
    }

    pub(crate) fn verifier(&self) -> &str {
        &self.verifier
    }

    /// The authorization code in the redirect, after checking it answers this attempt.
    pub fn code(&self, redirect: &str) -> Result<String> {
        let url = Url::parse(redirect).map_err(|_| Error::Failed("bad redirect".into()))?;
        let expected = Url::parse(REDIRECT).expect("constant url");
        if url.origin() != expected.origin() || url.path() != expected.path() {
            return Err(Error::Failed("unexpected redirect".into()));
        }
        let get = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        if get("state").as_deref() != Some(self.state.as_str()) {
            return Err(Error::Failed(
                "the answer belongs to another sign-in".into(),
            ));
        }
        if let Some(error) = get("error") {
            let answer = serde_json::json!({
                "error": error,
                "error_description": get("error_description"),
            });
            return Err(crate::error::from_answer(400, &answer, SPACES));
        }
        get("code")
            .filter(|code| !code.is_empty())
            .ok_or_else(|| Error::Failed("no code in the redirect".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_carries_pkce_but_not_the_verifier() {
        let sign_in = SignIn::new();
        let request = sign_in.request(Some("{\"a\":1}"));
        assert!(request.url.contains("code_challenge_method=S256"));
        assert!(request.url.contains("claims=%7B"));
        assert!(!request.url.contains(sign_in.verifier()));
        assert_eq!(sign_in.verifier().len(), 43);
        assert_ne!(SignIn::new().verifier(), sign_in.verifier());
    }

    #[test]
    fn only_our_own_answer_is_accepted() {
        let sign_in = SignIn::new();
        let good = format!("{REDIRECT}?code=0.A%2Fb&state={}", sign_in.state);
        assert_eq!(sign_in.code(&good).unwrap(), "0.A/b");
        for bad in [
            format!("{REDIRECT}?code=x&state=other"),
            format!("{REDIRECT}?code=x"),
            format!("{REDIRECT}?state={}", sign_in.state),
            format!(
                "https://evil.example/common/oauth2/nativeclient?code=x&state={}",
                sign_in.state
            ),
            format!(
                "https://login.microsoftonline.com/other?code=x&state={}",
                sign_in.state
            ),
        ] {
            assert!(sign_in.code(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_refusal_keeps_its_code() {
        let sign_in = SignIn::new();
        let url = format!(
            "{REDIRECT}?error=access_denied&error_description=AADSTS53003%3A%20no&state={}",
            sign_in.state
        );
        assert_eq!(
            sign_in.code(&url),
            Err(Error::Blocked("AADSTS53003".into()))
        );
    }
}
