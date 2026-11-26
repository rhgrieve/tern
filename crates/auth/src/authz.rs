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
    /// The account's partition within its region, such as `amer01`.
    pub partition: String,
    /// The host of every regional service named, by its name. Only hosts: no paths.
    pub hosts: Vec<(String, String)>,
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
    let hosts = gtms
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, url)| {
            let url = url::Url::parse(url.as_str()?).ok()?;
            let host = url.host_str()?.to_owned();
            Some((name.clone(), host))
        })
        .collect();
    Some(Regions {
        region: answer["region"].as_str().unwrap_or_default().to_owned(),
        presence,
        trouter_host,
        chat_service: checked("chatService", SPACES),
        partition: answer["partition"].as_str().unwrap_or_default().to_owned(),
        hosts,
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
