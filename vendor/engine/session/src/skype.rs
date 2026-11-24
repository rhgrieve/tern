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
pub(crate) struct Held(Option<(Grant, Instant)>);

#[derive(Clone)]
struct Grant {
    token: String,
    /// Where this account's region keeps pictures, ending in a slash.
    media: Option<String>,
}

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

fn parse(answer: &Value) -> Option<(Grant, Duration)> {
    let token = answer.pointer("/tokens/skypeToken")?.as_str()?;
    let expires = answer.pointer("/tokens/expiresIn")?;
    let seconds = expires
        .as_u64()
        .or_else(|| expires.as_str()?.parse().ok())?;
    let media = answer
        .pointer("/regionGtms/amsV2")
        .and_then(Value::as_str)
        .map(|url| format!("{}/", url.trim_end_matches('/')))
        .filter(|url| skype_token_allowed(url));
    let grant = Grant {
        token: token.to_owned(),
        media,
    };
    Some((grant, Duration::from_secs(seconds)))
}

fn spaces() -> Scope {
    Scope::new(SPACES, "authorization.readwrite")
}

fn media(grant: &Grant) -> Result<String> {
    grant
        .media
        .clone()
        .ok_or_else(|| Error::PageOnly("a picture upload in this region".into()))
}

fn skype_headers(grant: &Grant) -> Vec<(String, String)> {
    vec![
        (
            "Authorization".to_owned(),
            format!("skype_token {}", grant.token),
        ),
        (
            "x-ms-client-version".to_owned(),
            "1415/1.0.0.2018090100".to_owned(),
        ),
    ]
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
        let token = self.skype_grant(force).await?.token;
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

    /// Stores a picture that members of `conversation` can read. Answers its id and the address
    /// of the service that has it.
    pub async fn skype_upload(
        &self,
        conversation: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<(Value, String)> {
        let mut grant = self.skype_grant(false).await?;
        let mut created = self.skype_create(&grant, conversation, name).await?;
        if created.status == 401 {
            grant = self.skype_grant(true).await?;
            created = self.skype_create(&grant, conversation, name).await?;
        }
        let base = media(&grant)?;
        let id = created.body["id"]
            .as_str()
            .filter(|_| created.is_success())
            .filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
            .ok_or_else(|| Error::api(created.status, &base, Value::Null))?
            .to_owned();
        let url = format!("{base}v1/objects/{id}/content/imgpsh");
        let mut headers = skype_headers(&grant);
        headers.push((
            "Content-Type".to_owned(),
            "application/octet-stream".to_owned(),
        ));
        headers.push((
            "Content-Range".to_owned(),
            format!(
                "bytes 0-{}/{}",
                bytes.len().saturating_sub(1),
                bytes.len()
            ),
        ));
        let request = Request {
            method: Method::Put,
            headers,
            anonymous: true,
            body_base64: Some(
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            ),
            ..Request::get(&url)
        };
        let stored = self.once(&request, &spaces()).await?;
        if !stored.is_success() {
            return Err(Error::api(stored.status, &base, Value::Null));
        }
        Ok((serde_json::json!({ "id": id }), base))
    }

    async fn skype_create(
        &self,
        grant: &Grant,
        conversation: &str,
        name: &str,
    ) -> Result<crate::ApiResponse> {
        let url = format!("{}v1/objects/", media(grant)?);
        let mut permissions = serde_json::Map::new();
        permissions.insert(conversation.to_owned(), serde_json::json!(["read"]));
        let body = serde_json::json!({
            "type": "pish/image",
            "permissions": permissions,
            "sharingMode": "Inline",
            "filename": name,
        });
        let mut request = Request::with_body(Method::Post, &url, body);
        request.anonymous = true;
        request.headers = skype_headers(grant);
        request
            .headers
            .push(("Accept".to_owned(), "application/json".to_owned()));
        self.once(&request, &spaces()).await
    }

    async fn skype_grant(&self, force: bool) -> Result<Grant> {
        let mut held = self.skype.lock().await;
        if let Some((grant, until)) = &held.0
            && !force
            && Instant::now() < *until
        {
            return Ok(grant.clone());
        }
        let request = Request {
            method: Method::Post,
            ..Request::get(AUTHZ)
        };
        let scope = Scope::new(SPACES, "authorization.readwrite");
        let answer = self.once(&request, &scope).await?;
        // The answer holds the token, so it never becomes part of an error.
        let (grant, lifetime) = parse(&answer.body)
            .filter(|_| answer.is_success())
            .ok_or_else(|| Error::api(answer.status, AUTHZ, Value::Null))?;
        held.0 = Some((
            grant.clone(),
            Instant::now() + lifetime.saturating_sub(MARGIN),
        ));
        Ok(grant)
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
        let (grant, lifetime) = parse(&answer).unwrap();
        assert_eq!((grant.token.as_str(), lifetime), ("t", Duration::from_secs(100)));
        assert_eq!(grant.media, None);
        assert!(parse(&json!({})).is_none());
        let region = |url: &str| {
            let answer = json!({"tokens": {"skypeToken": "t", "expiresIn": 1}, "regionGtms": {"amsV2": url}});
            parse(&answer).unwrap().0.media
        };
        assert_eq!(
            region("https://us-prod.asyncgw.teams.microsoft.com").as_deref(),
            Some("https://us-prod.asyncgw.teams.microsoft.com/")
        );
        assert_eq!(region("https://evil.example"), None, "the token stays home");
    }
}
