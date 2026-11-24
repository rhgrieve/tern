//! Holds the refresh token and mints one access token per audience.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use session::{BoxFuture, Scope, TokenProvider};
use tokio::sync::Mutex;

use crate::error::from_answer;
use crate::flow::{AUTHORITY, REDIRECT, SignIn, scope};
use crate::{
    CLIENT_ID, Error, Identity, Regions, Result, SPACES, SecretStore, StoredLogin, parse_regions,
};

const MARGIN: Duration = Duration::from_secs(5 * 60);
/// A forced refresh is answered with a token this young: several requests fail together when a
/// token is revoked, and one new token serves them all.
const FRESH: Duration = Duration::from_secs(20);
const RETRIES: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
const AUTHZ: &str = "https://teams.cloud.microsoft/api/authsvc/v1.0/authz";

#[derive(Clone)]
pub struct TokenReply {
    pub status: u16,
    pub body: Value,
}

/// The sign-in service, replaced in tests.
pub trait Endpoint: Send + Sync {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a [(&'a str, &'a str)],
    ) -> BoxFuture<'a, Result<TokenReply>>;
    fn post_bearer<'a>(&'a self, url: &'a str, token: &'a str)
    -> BoxFuture<'a, Result<TokenReply>>;
}

pub struct HttpEndpoint(reqwest::Client);

impl HttpEndpoint {
    pub fn new() -> Result<Self> {
        session::ensure_crypto_provider();
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .build()
            .map(HttpEndpoint)
            .map_err(|error| Error::Failed(error.to_string()))
    }
}

async fn reply(request: reqwest::RequestBuilder) -> Result<TokenReply> {
    // `without_url` keeps query strings out of error texts.
    let network = |error: reqwest::Error| Error::Unavailable(error.without_url().to_string());
    let response = request.send().await.map_err(network)?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(network)?;
    Ok(TokenReply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
    })
}

impl Endpoint for HttpEndpoint {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a [(&'a str, &'a str)],
    ) -> BoxFuture<'a, Result<TokenReply>> {
        Box::pin(reply(self.0.post(url).form(form)))
    }

    fn post_bearer<'a>(
        &'a self,
        url: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<TokenReply>> {
        Box::pin(reply(
            self.0
                .post(url)
                .bearer_auth(token)
                .header("Content-Length", 0),
        ))
    }
}

struct Held {
    token: String,
    minted: Instant,
    expires: Instant,
}

#[derive(Default)]
struct State {
    login: Option<StoredLogin>,
    /// Keyed by tenant and audience.
    tokens: HashMap<(String, String), Held>,
}

/// One lock guards everything, so there is never more than one refresh in flight: the refresh
/// token rotates with every use, and two at once could invalidate each other.
pub struct Broker {
    me: Weak<Broker>,
    endpoint: Box<dyn Endpoint>,
    store: Box<dyn SecretStore>,
    state: Mutex<State>,
}

impl Broker {
    pub fn new(endpoint: Box<dyn Endpoint>, store: Box<dyn SecretStore>) -> Arc<Self> {
        Arc::new_cyclic(|me| Broker {
            me: me.clone(),
            endpoint,
            store,
            state: Mutex::default(),
        })
    }

    /// Reads the stored sign-in; `None` when nobody is signed in.
    pub async fn load(&self) -> Result<Option<Identity>> {
        let login = self.store.load().await?;
        let mut state = self.state.lock().await;
        state.tokens.clear();
        state.login = login;
        Ok(state.login.as_ref().map(|login| login.identity.clone()))
    }

    pub async fn identity(&self) -> Option<Identity> {
        self.state
            .lock()
            .await
            .login
            .as_ref()
            .map(|login| login.identity.clone())
    }

    /// Redeems the code from the sign-in window and stores the login.
    pub async fn sign_in(&self, attempt: &SignIn, redirect: &str) -> Result<Identity> {
        let code = attempt.code(redirect)?;
        let answer = self
            .request(
                "organizations",
                SPACES,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", REDIRECT),
                    ("code_verifier", attempt.verifier()),
                ],
            )
            .await?;
        let identity = answer["id_token"]
            .as_str()
            .and_then(Identity::from_id_token)
            .ok_or_else(|| Error::Failed("no identity in the answer".into()))?;
        let refresh_token = answer["refresh_token"]
            .as_str()
            .ok_or_else(|| Error::Failed("no refresh token in the answer".into()))?
            .to_owned();
        let login = StoredLogin {
            refresh_token,
            identity: identity.clone(),
        };
        self.store.save(&login).await?;
        let mut state = self.state.lock().await;
        state.tokens.clear();
        state.login = Some(login);
        Ok(identity)
    }

    pub async fn sign_out(&self) -> Result<()> {
        *self.state.lock().await = State::default();
        self.store.clear().await
    }

    /// An access token for `resource`, in the home tenant or in `tenant`.
    ///
    /// Runs as its own task: a caller that gives up must not cut off a refresh whose answer holds
    /// the next refresh token.
    pub async fn access(
        &self,
        resource: &str,
        tenant: Option<&str>,
        force: bool,
    ) -> Result<String> {
        let me = self.me.upgrade().ok_or(Error::SignedOut)?;
        let (resource, tenant) = (resource.to_owned(), tenant.map(str::to_owned));
        tokio::spawn(async move { me.mint(&resource, tenant.as_deref(), force).await })
            .await
            .map_err(|error| Error::Failed(error.to_string()))?
    }

    async fn mint(&self, resource: &str, tenant: Option<&str>, force: bool) -> Result<String> {
        let mut state = self.state.lock().await;
        let login = state.login.clone().ok_or(Error::SignedOut)?;
        let tenant = tenant.unwrap_or(&login.identity.tenant_id).to_owned();
        let key = (tenant.clone(), resource.to_owned());
        if let Some(held) = state.tokens.get(&key) {
            let now = Instant::now();
            let usable = held.expires.saturating_duration_since(now) > MARGIN;
            if usable && (!force || now.duration_since(held.minted) < FRESH) {
                return Ok(held.token.clone());
            }
        }
        let answer = self
            .request(
                &tenant,
                resource,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &login.refresh_token),
                ],
            )
            .await?;
        let token = answer["access_token"]
            .as_str()
            .ok_or_else(|| Error::Failed("no token in the answer".into()))?
            .to_owned();
        let lifetime = answer["expires_in"]
            .as_u64()
            .or_else(|| answer["expires_in"].as_str()?.parse().ok())
            .unwrap_or(3600);
        if let Some(rotated) = answer["refresh_token"]
            .as_str()
            .filter(|new| *new != login.refresh_token)
        {
            let login = StoredLogin {
                refresh_token: rotated.to_owned(),
                ..login
            };
            // A keyring hiccup must not cost the session: the new token stays in memory and the
            // previous one, still in the keyring, normally stays valid.
            if let Err(error) = self.store.save(&login).await {
                tracing::warn!(%error, "could not store the rotated sign-in");
            }
            state.login = Some(login);
        }
        let now = Instant::now();
        state.tokens.insert(
            key,
            Held {
                token: token.clone(),
                minted: now,
                expires: now + Duration::from_secs(lifetime),
            },
        );
        Ok(token)
    }

    /// The regional hosts for this account.
    pub async fn regions(&self) -> Result<Regions> {
        let token = self.access(SPACES, None, false).await?;
        let reply = self.endpoint.post_bearer(AUTHZ, &token).await?;
        match reply.status {
            200..=299 => parse_regions(&reply.body).ok_or_else(|| {
                Error::Failed("unexpected answer from the Teams sign-in service".into())
            }),
            status @ (429 | 500..) => Err(Error::Unavailable(format!("HTTP {status}"))),
            status => Err(Error::Failed(format!(
                "Teams sign-in service: HTTP {status}"
            ))),
        }
    }

    async fn request(&self, tenant: &str, resource: &str, grant: &[(&str, &str)]) -> Result<Value> {
        if !tenant
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        {
            return Err(Error::Failed("bad tenant".into()));
        }
        let url = format!("{AUTHORITY}/{tenant}/oauth2/v2.0/token");
        let scope = scope(resource);
        let mut form = grant.to_vec();
        form.extend([("client_id", CLIENT_ID), ("scope", scope.as_str())]);
        // A code works once, so only a refresh is tried again.
        let refresh = grant.contains(&("grant_type", "refresh_token"));
        let mut pauses = RETRIES.iter();
        loop {
            let error = match self.endpoint.post_form(&url, &form).await {
                Ok(reply) if (200..300).contains(&reply.status) => return Ok(reply.body),
                Ok(reply) => from_answer(reply.status, &reply.body, resource),
                Err(error) => error,
            };
            match (&error, pauses.next()) {
                (Error::Unavailable(_), Some(pause)) if refresh => tokio::time::sleep(*pause).await,
                _ => return Err(error),
            }
        }
    }
}

impl TokenProvider for Broker {
    fn token<'a>(
        &'a self,
        scope: &'a Scope,
        tenant: Option<&'a str>,
        force: bool,
    ) -> BoxFuture<'a, session::Result<String>> {
        Box::pin(async move {
            self.access(&scope.resource, tenant, force)
                .await
                .map_err(|error| match error {
                    Error::SignedOut | Error::InteractionRequired { .. } | Error::Blocked(_) => {
                        session::Error::LoginRequired(error.to_string())
                    }
                    error => session::Error::NoFreshToken {
                        scope: scope.to_string(),
                        detail: format!(" ({error})"),
                    },
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::jwt::fake;
    use serde_json::json;

    #[derive(Default)]
    struct Fake {
        replies: StdMutex<Vec<TokenReply>>,
        refresh_tokens: StdMutex<Vec<String>>,
        urls: StdMutex<Vec<String>>,
        calls: AtomicUsize,
    }

    impl Endpoint for Arc<Fake> {
        fn post_form<'a>(
            &'a self,
            url: &'a str,
            form: &'a [(&'a str, &'a str)],
        ) -> BoxFuture<'a, Result<TokenReply>> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                let count = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                self.urls.lock().unwrap().push(url.to_owned());
                if let Some((_, token)) = form.iter().find(|(name, _)| *name == "refresh_token") {
                    self.refresh_tokens
                        .lock()
                        .unwrap()
                        .push((*token).to_owned());
                }
                let mut replies = self.replies.lock().unwrap();
                Ok(if replies.is_empty() {
                    TokenReply {
                        status: 200,
                        body: json!({
                            "access_token": format!("access-{count}"), "expires_in": 3600,
                            "refresh_token": format!("refresh-{count}"),
                            "id_token": fake(json!({"oid": "o", "tid": "tenant-1", "name": "Alex"})),
                        }),
                    }
                } else {
                    replies.remove(0)
                })
            })
        }

        fn post_bearer<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<TokenReply>> {
            Box::pin(async {
                Ok(TokenReply {
                    status: 200,
                    body: json!({}),
                })
            })
        }
    }

    #[derive(Default)]
    struct Memory {
        login: StdMutex<Option<StoredLogin>>,
        broken: AtomicBool,
    }

    impl SecretStore for Arc<Memory> {
        fn load(&self) -> BoxFuture<'_, Result<Option<StoredLogin>>> {
            Box::pin(async { Ok(self.login.lock().unwrap().clone()) })
        }
        fn save<'a>(&'a self, login: &'a StoredLogin) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                if self.broken.load(Ordering::SeqCst) {
                    return Err(Error::Keyring("locked".into()));
                }
                *self.login.lock().unwrap() = Some(login.clone());
                Ok(())
            })
        }
        fn clear(&self) -> BoxFuture<'_, Result<()>> {
            Box::pin(async {
                *self.login.lock().unwrap() = None;
                Ok(())
            })
        }
    }

    async fn signed_in() -> (Arc<Broker>, Arc<Fake>, Arc<Memory>) {
        let (fake, memory) = (Arc::new(Fake::default()), Arc::new(Memory::default()));
        *memory.login.lock().unwrap() = Some(StoredLogin {
            refresh_token: "refresh-0".into(),
            identity: Identity {
                object_id: "o".into(),
                tenant_id: "tenant-1".into(),
                name: "Alex".into(),
            },
        });
        let broker = Broker::new(Box::new(fake.clone()), Box::new(memory.clone()));
        broker.load().await.unwrap();
        (broker, fake, memory)
    }

    fn failure(status: u16, error: &str) -> TokenReply {
        TokenReply {
            status,
            body: json!({"error": error}),
        }
    }

    #[tokio::test]
    async fn concurrent_requests_share_one_refresh() {
        let (broker, fake, _) = signed_in().await;
        let (a, b, c) = tokio::join!(
            broker.access("https://r", None, false),
            broker.access("https://r", None, false),
            broker.access("https://r", None, true),
        );
        assert_eq!(
            (a.unwrap(), b.unwrap(), c.unwrap()),
            ("access-1".into(), "access-1".into(), "access-1".into())
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_caller_that_gives_up_does_not_lose_the_rotation() {
        let (broker, fake, memory) = signed_in().await;
        {
            let mut access = Box::pin(broker.access("https://a", None, false));
            // Polled once, so the refresh has started, then dropped.
            assert!(futures_poll(&mut access).await);
        }
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            memory.login.lock().unwrap().as_ref().unwrap().refresh_token,
            "refresh-1"
        );
    }

    /// Polls once and answers whether the future is still pending.
    async fn futures_poll<F: Future + Unpin>(future: &mut F) -> bool {
        std::future::poll_fn(|context| {
            std::task::Poll::Ready(std::pin::Pin::new(&mut *future).poll(context).is_pending())
        })
        .await
    }

    #[tokio::test]
    async fn rotation_is_stored_and_used_next() {
        let (broker, fake, memory) = signed_in().await;
        broker.access("https://a", None, false).await.unwrap();
        assert_eq!(
            memory.login.lock().unwrap().as_ref().unwrap().refresh_token,
            "refresh-1"
        );
        broker.access("https://b", None, false).await.unwrap();
        assert_eq!(
            *fake.refresh_tokens.lock().unwrap(),
            ["refresh-0", "refresh-1"]
        );
    }

    #[tokio::test]
    async fn a_locked_keyring_does_not_lose_the_session() {
        let (broker, fake, memory) = signed_in().await;
        memory.broken.store(true, Ordering::SeqCst);
        assert!(broker.access("https://a", None, false).await.is_ok());
        broker.access("https://b", None, false).await.unwrap();
        assert_eq!(fake.refresh_tokens.lock().unwrap()[1], "refresh-1");
    }

    #[tokio::test(start_paused = true)]
    async fn an_outage_is_retried_but_a_dead_token_is_not() {
        let (broker, fake, _) = signed_in().await;
        *fake.replies.lock().unwrap() = vec![failure(503, "x"), failure(429, "x")];
        assert!(broker.access("https://a", None, false).await.is_ok());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
        *fake.replies.lock().unwrap() = vec![failure(400, "invalid_grant")];
        assert_eq!(
            broker.access("https://b", None, false).await,
            Err(Error::SignedOut)
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
        // The stored sign-in is kept: only the user signs out.
        assert!(broker.identity().await.is_some());
    }

    #[tokio::test]
    async fn guest_tenants_have_their_own_tokens() {
        let (broker, fake, _) = signed_in().await;
        let home = broker.access("https://a", None, false).await.unwrap();
        let guest = broker
            .access("https://a", Some("tenant-2"), false)
            .await
            .unwrap();
        assert_ne!(home, guest);
        assert!(fake.urls.lock().unwrap()[1].contains("/tenant-2/"));
        assert!(
            broker
                .access("https://a", Some("../evil"), false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn signing_out_forgets_everything() {
        let (broker, _, memory) = signed_in().await;
        broker.access("https://a", None, false).await.unwrap();
        broker.sign_out().await.unwrap();
        assert!(memory.login.lock().unwrap().is_none());
        assert_eq!(
            broker.access("https://a", None, false).await,
            Err(Error::SignedOut)
        );
    }

    #[test]
    fn debug_output_hides_the_refresh_token() {
        let login = StoredLogin {
            refresh_token: "secret-value".into(),
            identity: Identity {
                object_id: "o".into(),
                tenant_id: "t".into(),
                name: "n".into(),
            },
        };
        assert!(!format!("{login:?}").contains("secret-value"));
    }
}
