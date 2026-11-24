//! The Teams authz service: which regional hosts serve this account.

use serde_json::Value;
use session::token_allowed;

use crate::SPACES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    pub region: String,
    /// The presence service, ending in `/v1`.
    pub presence: Option<String>,
    pub trouter_host: Option<String>,
    pub chat_service: Option<String>,
}

const PRESENCE: &str = "https://presence.teams.microsoft.com";

/// The answer decides where tokens are sent, so every host in it is checked.
pub fn parse_regions(answer: &Value) -> Option<Regions> {
    let gtms = answer.get("regionGtms")?;
    let checked = |name: &str, audience: &str| {
        gtms[name]
            .as_str()
            .map(|url| url.trim_end_matches('/').to_owned())
            .filter(|url| token_allowed(audience, url))
    };
    let presence = checked("presenceUPS", PRESENCE).map(|base| {
        if base.ends_with("/v1") {
            base
        } else {
            format!("{base}/v1")
        }
    });
    let trouter_host = gtms["calling_trouterUrl"]
        .as_str()
        .and_then(|url| url::Url::parse(url).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| host.ends_with(".trouter.teams.microsoft.com"));
    Some(Regions {
        region: answer["region"].as_str().unwrap_or_default().to_owned(),
        presence,
        trouter_host,
        chat_service: checked("chatService", SPACES),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_hosts_and_refuses_foreign_ones() {
        let answer = json!({"region": "amer", "regionGtms": {
            "presenceUPS": "https://teams.microsoft.com/ups/amer/",
            "calling_trouterUrl": "https://go-us.trouter.teams.microsoft.com/v4/a",
            "chatService": "https://evil.example/",
        }});
        let regions = parse_regions(&answer).unwrap();
        assert_eq!(
            regions.presence.as_deref(),
            Some("https://teams.microsoft.com/ups/amer/v1")
        );
        assert_eq!(
            regions.trouter_host.as_deref(),
            Some("go-us.trouter.teams.microsoft.com")
        );
        assert_eq!(regions.chat_service, None);
        assert!(parse_regions(&json!({})).is_none());
    }
}
