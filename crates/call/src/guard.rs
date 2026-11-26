//! What a call may ask of Teams. Everything else is refused before it leaves the computer, so a
//! mistake in the call code cannot read or change the rest of the account.

use session::{Method, Request};

/// The Echo bot, the only one the call may start a chat with.
const ECHO_BOT: &str = "28:cf28171e-fcfd-47e4-a1d6-79460b0b3ca0";

pub fn allows(request: &Request, region: &str) -> bool {
    let Ok(url) = url::Url::parse(&request.url) else {
        return false;
    };
    let plain = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none();
    let Some(host) = url.host_str().filter(|_| plain) else {
        return false;
    };
    let path = url.path();
    let get = request.method == Method::Get;
    let chats = format!("/api/chatsvc/{region}/v1/");
    match host {
        // Who we are.
        "graph.microsoft.com" => get && path == "/v1.0/me",
        // The relay credential.
        "teams.microsoft.com" => get && path == "/trap/tokens",
        // The push endpoint the call's answers come to.
        "teams.cloud.microsoft" if path.starts_with("/registrar/prod/V2/registrations") => true,
        // Looking for the chat with the Echo bot, and starting it the first time.
        "teams.cloud.microsoft" if path.starts_with(&chats) => {
            let rest = &path[chats.len()..];
            let echo_chat =
                rest.starts_with("users/ME/conversations/") && rest.contains("unq.gbl.spaces");
            let new_echo_chat = request.method == Method::Post
                && rest == "threads"
                && request
                    .body
                    .as_ref()
                    .is_some_and(|body| body.to_string().contains(ECHO_BOT));
            (get && echo_chat) || new_echo_chat
        }
        // Teams' call proxy: everything about the call itself.
        host => host.ends_with(".flightproxy.teams.microsoft.com"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn get(url: &str) -> Request {
        Request::get(url)
    }

    fn post(url: &str, body: serde_json::Value) -> Request {
        Request::with_body(Method::Post, url, body)
    }

    #[test]
    fn a_call_reaches_what_it_needs() {
        for request in [
            get("https://graph.microsoft.com/v1.0/me?$select=id,displayName"),
            get("https://teams.microsoft.com/trap/tokens"),
            post(
                "https://teams.cloud.microsoft/registrar/prod/V2/registrations",
                json!({}),
            ),
            post(
                "https://api-noam.flightproxy.teams.microsoft.com/api/v2/epconv",
                json!({}),
            ),
            get(
                "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/19%3Aa_b%40unq.gbl.spaces?view=msnp24Equivalent",
            ),
            post(
                "https://teams.cloud.microsoft/api/chatsvc/amer/v1/threads",
                json!({"members": [{"id": "8:orgid:me"}, {"id": ECHO_BOT}]}),
            ),
        ] {
            assert!(allows(&request, "amer"), "{}", request.url);
        }
    }

    #[test]
    fn the_rest_of_the_account_is_refused() {
        for request in [
            get("https://graph.microsoft.com/v1.0/me/messages"),
            get("https://graph.microsoft.com/v1.0/me/chats"),
            post("https://teams.microsoft.com/trap/tokens", json!({})),
            get("https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations"),
            get(
                "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/19%3Aabc%40thread.v2/messages",
            ),
            post(
                "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/19%3Aa_b%40unq.gbl.spaces/messages",
                json!({"content": ECHO_BOT}),
            ),
            post(
                "https://teams.cloud.microsoft/api/chatsvc/amer/v1/threads",
                json!({"members": [{"id": "8:orgid:me"}, {"id": "8:orgid:someone"}]}),
            ),
            get(
                "https://teams.cloud.microsoft/api/chatsvc/emea/v1/users/ME/conversations/19%3Aa_b%40unq.gbl.spaces",
            ),
            get("https://evil.example/#.flightproxy.teams.microsoft.com"),
            get("https://api.flightproxy.teams.microsoft.com:8443/x"),
            get("http://api.flightproxy.teams.microsoft.com/x"),
            get("https://us-api.asm.skype.com/v1/objects"),
        ] {
            assert!(!allows(&request, "amer"), "{}", request.url);
        }
    }
}
