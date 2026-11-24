//! Tern patch: a session without a browser. Tokens come from a [`TokenProvider`], and a bearer
//! is only ever attached to a host that belongs to the token's audience.

use url::Url;

use crate::app::App;
use crate::error::{Error, Result};
use crate::scope::Scope;
use crate::transport::{BoxFuture, Diagnosis, OpenedTab, Transport};

/// Mints access tokens. Implementations cache and refresh; `force` asks for a new token because
/// the service refused the current one.
pub trait TokenProvider: Send + Sync {
    fn token<'a>(
        &'a self,
        scope: &'a Scope,
        tenant: Option<&'a str>,
        force: bool,
    ) -> BoxFuture<'a, Result<String>>;
}

/// Stands in for the browser: there are no tabs to open.
pub(crate) struct NoBrowser;

impl Transport for NoBrowser {
    fn check(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn open(&self, _app: App, _with_events: bool) -> BoxFuture<'_, Result<Option<OpenedTab>>> {
        Box::pin(async { Ok(None) })
    }

    fn wake(&self, _app: App) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn diagnose(&self) -> BoxFuture<'_, Result<Diagnosis>> {
        Box::pin(async { Ok(Diagnosis::default()) })
    }
}

/// Domains whose hosts (the domain itself or a subdomain) may see a token for `resource`.
/// An audience that is not listed may only be sent to its own host.
fn audience_domains(resource: &str) -> &'static [&'static str] {
    match resource.trim_end_matches('/') {
        "https://graph.microsoft.com" => &["graph.microsoft.com"],
        "https://presence.teams.microsoft.com" => &[
            "presence.teams.microsoft.com",
            "teams.microsoft.com",
            "teams.cloud.microsoft",
        ],
        "https://ic3.teams.office.com"
        | "https://api.spaces.skype.com"
        | "https://chatsvcagg.teams.microsoft.com"
        | "https://uis.teams.microsoft.com" => &[
            "teams.microsoft.com",
            "teams.cloud.microsoft",
            "api.spaces.skype.com",
            "ic3.teams.office.com",
            "asm.skype.com",
        ],
        "https://outlook.office.com" => &[
            "outlook.office.com",
            "outlook.cloud.microsoft",
            "substrate.office.com",
        ],
        "https://substrate.office.com" => &["substrate.office.com"],
        _ => &[],
    }
}

fn within(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// Whether a token for `resource` may be sent to `url`.
pub fn token_allowed(resource: &str, url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return false;
    }
    let listed = audience_domains(resource);
    if !listed.is_empty() {
        return listed.iter().any(|domain| within(host, domain));
    }
    Url::parse(resource).is_ok_and(|resource| resource.host_str() == Some(host))
}

pub(crate) fn page_only(what: &str) -> Error {
    Error::PageOnly(what.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPH: &str = "https://graph.microsoft.com";
    const IC3: &str = "https://ic3.teams.office.com";
    const PRESENCE: &str = "https://presence.teams.microsoft.com";

    #[test]
    fn tokens_go_to_their_own_services() {
        for (resource, url) in [
            (GRAPH, "https://graph.microsoft.com/v1.0/me"),
            (
                IC3,
                "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME",
            ),
            (IC3, "https://amer.ng.msg.teams.microsoft.com/v1/users/ME"),
            (
                PRESENCE,
                "https://presence.teams.microsoft.com/v1/me/endpoints/",
            ),
            (
                PRESENCE,
                "https://teams.microsoft.com/ups/amer/v1/me/endpoints/",
            ),
            (
                "https://contoso.sharepoint.com",
                "https://contoso.sharepoint.com/_api/x",
            ),
        ] {
            assert!(token_allowed(resource, url), "{resource} -> {url}");
        }
    }

    struct Never;

    impl TokenProvider for Never {
        fn token<'a>(
            &'a self,
            _: &'a Scope,
            _: Option<&'a str>,
            _: bool,
        ) -> BoxFuture<'a, Result<String>> {
            panic!("a token was asked for");
        }
    }

    #[tokio::test]
    async fn a_session_refuses_foreign_hosts_before_asking_for_a_token() {
        use crate::{Method, Request, Session, SessionConfig};
        let session = Session::with_provider(std::sync::Arc::new(Never), SessionConfig::default());
        let sent = session
            .request(
                Method::Get,
                "https://evil.example/?x=1",
                &Scope::graph("User.Read"),
                None,
            )
            .await;
        assert!(
            matches!(sent, Err(Error::TokenHostRefused { url, .. }) if url == "https://evil.example/")
        );
        let scope = Scope::graph("User.Read");
        let augloop = Request::augloop(
            "https://graph.microsoft.com/x",
            crate::AugloopExchange {
                stages: vec![],
                placeholder: String::new(),
                done_marker: String::new(),
                timeout_ms: 1,
                keepalive_ms: 1,
            },
        );
        assert!(matches!(
            session.send(augloop, &scope).await,
            Err(Error::PageOnly(_))
        ));
        assert!(matches!(
            session
                .run_with_token(App::Teams, &scope, "", &serde_json::json!({}), false)
                .await,
            Err(Error::PageOnly(_))
        ));
    }

    #[test]
    fn tokens_never_go_elsewhere() {
        for (resource, url) in [
            (GRAPH, "https://graph.microsoft.com.evil.example/v1.0/me"),
            (GRAPH, "https://evilgraph.microsoft.com/v1.0/me"),
            (GRAPH, "https://teams.microsoft.com/x"),
            (GRAPH, "http://graph.microsoft.com/v1.0/me"),
            (GRAPH, "https://graph.microsoft.com:8443/v1.0/me"),
            (GRAPH, "https://graph.microsoft.com@evil.example/"),
            (GRAPH, "https://user@graph.microsoft.com/"),
            (IC3, "https://teams.microsoft.com.evil.example/"),
            (IC3, "https://notteams.microsoft.com/"),
            (IC3, "https://graph.microsoft.com/v1.0/me"),
            (
                "https://contoso.sharepoint.com",
                "https://fabrikam.sharepoint.com/",
            ),
            (GRAPH, "not a url"),
        ] {
            assert!(!token_allowed(resource, url), "{resource} -> {url}");
        }
    }
}
