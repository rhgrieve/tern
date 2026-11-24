use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not signed in")]
    SignedOut,
    /// The tenant wants the user to act (MFA, consent, a claims challenge).
    #[error("sign-in needs you again ({code})")]
    InteractionRequired {
        code: String,
        claims: Option<String>,
    },
    /// Tenant policy refuses this client or device; signing in again will not help.
    #[error("blocked by your organization's policy ({0})")]
    Blocked(String),
    #[error("this sign-in is not allowed to use {resource} ({code})")]
    NotGranted { resource: String, code: String },
    #[error("sign-in service unavailable: {0}")]
    Unavailable(String),
    #[error("sign-in failed: {0}")]
    Failed(String),
    #[error("keyring: {0}")]
    Keyring(String),
}

/// Codes from Microsoft's sign-in service that a new sign-in cannot fix.
const BLOCKED: [&str; 7] = [
    "AADSTS53000",
    "AADSTS53001",
    "AADSTS53002",
    "AADSTS53003",
    "AADSTS530003",
    "AADSTS50105",
    "AADSTS7000112",
];
const NOT_GRANTED: [&str; 4] = ["AADSTS65001", "AADSTS65002", "AADSTS650057", "AADSTS500011"];

/// The `AADSTS…` code in an error description. The rest of the text holds ids and is not kept.
fn aadsts(answer: &Value) -> Option<String> {
    let text = answer["error_description"].as_str()?;
    let start = text.find("AADSTS")?;
    let digits = text[start + 6..]
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    (digits > 0).then(|| text[start..start + 6 + digits].to_owned())
}

pub(crate) fn from_answer(status: u16, answer: &Value, resource: &str) -> Error {
    let error = answer["error"].as_str().unwrap_or("");
    let code = aadsts(answer).unwrap_or_else(|| {
        if error.is_empty() {
            format!("HTTP {status}")
        } else {
            error.to_owned()
        }
    });
    if status == 429 || status >= 500 || error == "temporarily_unavailable" {
        return Error::Unavailable(code);
    }
    if BLOCKED.contains(&code.as_str()) {
        return Error::Blocked(code);
    }
    if NOT_GRANTED.contains(&code.as_str())
        || matches!(error, "invalid_resource" | "consent_required")
    {
        return Error::NotGranted {
            resource: resource.to_owned(),
            code,
        };
    }
    match error {
        // A conditional-access demand arrives as invalid_grant with a suberror or claims.
        "invalid_grant" if answer["claims"].is_string() => Error::InteractionRequired {
            code,
            claims: answer["claims"].as_str().map(str::to_owned),
        },
        "invalid_grant" => Error::SignedOut,
        "interaction_required" | "login_required" => Error::InteractionRequired {
            code,
            claims: answer["claims"].as_str().map(str::to_owned),
        },
        _ => Error::Failed(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(status: u16, error: &str, description: &str) -> Error {
        from_answer(
            status,
            &json!({"error": error, "error_description": description}),
            "https://r",
        )
    }

    #[test]
    fn only_a_dead_refresh_token_signs_out() {
        assert_eq!(
            error(400, "invalid_grant", "AADSTS70008: expired"),
            Error::SignedOut
        );
        assert_eq!(
            error(503, "invalid_grant", "AADSTS70008"),
            Error::Unavailable("AADSTS70008".into())
        );
        assert_eq!(error(429, "x", ""), Error::Unavailable("x".into()));
        assert!(matches!(
            error(400, "invalid_scope", "AADSTS70011"),
            Error::Failed(_)
        ));
    }

    #[test]
    fn policy_and_consent_are_told_apart() {
        assert_eq!(
            error(400, "invalid_grant", "AADSTS53003: Blocked by CA"),
            Error::Blocked("AADSTS53003".into())
        );
        assert!(matches!(
            error(400, "invalid_grant", "AADSTS65002: consent"),
            Error::NotGranted { .. }
        ));
        assert!(matches!(
            error(400, "interaction_required", "AADSTS50076: MFA"),
            Error::InteractionRequired { .. }
        ));
    }

    #[test]
    fn a_claims_challenge_keeps_the_claims() {
        let answer = json!({"error": "invalid_grant", "error_description": "AADSTS50076", "claims": "{\"x\":1}"});
        assert_eq!(
            from_answer(400, &answer, "r"),
            Error::InteractionRequired {
                code: "AADSTS50076".into(),
                claims: Some("{\"x\":1}".into())
            }
        );
    }

    #[test]
    fn the_description_itself_is_not_kept() {
        let shown = error(400, "weird", "AADSTS12345: user alex@contoso.com trace 99").to_string();
        assert!(shown.contains("AADSTS12345") && !shown.contains("alex"));
    }
}
