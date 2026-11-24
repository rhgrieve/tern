//! Tern patch: the Skype token some Teams services still take, without a page.

use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::request::{Method, Request};
use crate::scope::{SPACES, Scope};
use crate::session::Session;

const AUTHZ: &str = "https://teams.cloud.microsoft/api/authsvc/v1.0/authz";
const MARGIN: Duration = Duration::from_secs(60);
/// Hosts that may see the token (a subdomain of one of these).
const DOMAINS: [&str; 2] = [".asm.skype.com", ".asyncgw.teams.microsoft.com"];

#[derive(Default)]
pub(crate) struct Held(Option<(String, Instant)>);

/// Whether the Skype token may be sent to `url`.
pub fn skype_token_allowed(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    let plain = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none();
    plain
        && url.host_str().is_some_and(|host| {
            DOMAINS
                .iter()
                .any(|domain| host.len() > domain.len() && host.ends_with(domain))
        })
}

fn parse(answer: &Value) -> Option<(String, Duration)> {
    let token = answer.pointer("/tokens/skypeToken")?.as_str()?;
    let expires = answer.pointer("/tokens/expiresIn")?;
    let seconds = expires
        .as_u64()
        .or_else(|| expires.as_str()?.parse().ok())?;
    Some((token.to_owned(), Duration::from_secs(seconds)))
}

impl Session {
    /// Whether requests go out without a browser.
    pub fn is_native(&self) -> bool {
        self.provider.is_some()
    }

    /// Downloads `url` with the Skype token; the answer is shaped like any binary one.
    pub async fn skype_download(&self, url: &str) -> Result<crate::ApiResponse> {
        if !skype_token_allowed(url) {
            return Err(Error::TokenHostRefused {
                resource: "Skype".into(),
                url: url.split('?').next().unwrap_or_default().to_owned(),
            });
        }
        let first = self.skype_get(url, false).await?;
        if first.status != 401 {
            return Ok(first);
        }
        self.skype_get(url, true).await
    }

    async fn skype_get(&self, url: &str, force: bool) -> Result<crate::ApiResponse> {
        let token = self.skype_token(force).await?;
        let headers = vec![
            ("Authorization".to_owned(), format!("skype_token {token}")),
            (
                "x-ms-client-version".to_owned(),
                "1415/1.0.0.2018090100".to_owned(),
            ),
        ];
        let request = Request::anonymous_binary_get(url, headers);
        let scope = Scope::new(SPACES, "authorization.readwrite");
        Ok(self.once(&request, &scope).await?)
    }

    async fn skype_token(&self, force: bool) -> Result<String> {
        let mut held = self.skype.lock().await;
        if let Some((token, until)) = &held.0
            && !force
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        let request = Request {
            method: Method::Post,
            ..Request::get(AUTHZ)
        };
        let scope = Scope::new(SPACES, "authorization.readwrite");
        let answer = self.once(&request, &scope).await?;
        // The answer holds the token, so it never becomes part of an error.
        let (token, lifetime) = parse(&answer.body)
            .filter(|_| answer.is_success())
            .ok_or_else(|| Error::api(answer.status, AUTHZ, Value::Null))?;
        held.0 = Some((
            token.clone(),
            Instant::now() + lifetime.saturating_sub(MARGIN),
        ));
        Ok(token)
    }

    async fn once(&self, request: &Request, scope: &Scope) -> Result<crate::ApiResponse> {
        Ok(self.batch(std::slice::from_ref(request), scope).await?.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_token_only_goes_to_teams_media_hosts() {
        for good in [
            "https://us-api.asm.skype.com/v1/objects/0-eus-d1/views/imgo",
            "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/x/views/imgo",
        ] {
            assert!(skype_token_allowed(good), "{good}");
        }
        for bad in [
            "http://us-api.asm.skype.com/x",
            "https://asm.skype.com.evil.example/x",
            "https://evilasm.skype.com/x",
            "https://.asm.skype.com/x",
            "https://us-api.asm.skype.com:8443/x",
            "https://us-api.asm.skype.com@evil.example/x",
            "https://a@us-api.asm.skype.com/x",
            "https://graph.microsoft.com/x",
            "nonsense",
        ] {
            assert!(!skype_token_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn reads_the_token_and_its_lifetime() {
        let answer = json!({"tokens": {"skypeToken": "t", "expiresIn": 100}});
        assert_eq!(parse(&answer), Some(("t".into(), Duration::from_secs(100))));
        assert_eq!(parse(&json!({})), None);
    }
}
