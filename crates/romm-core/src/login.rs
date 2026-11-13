//! Signing in: trade a pairing code for a token (or take a token as given), check that it works,
//! and say what is wrong with it if anything. The desktop app and the CLI both come through here,
//! so they sign in the same way and say the same things when it goes wrong.
//!
//! What a successful sign-in changes is left to the caller (see `Config::set_server` and
//! `store_token`): the two keep their configuration differently.

use crate::api::{missing_scopes, RommClient};
use crate::error::{Error, Result};

/// What the user signs in with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credentials<'a> {
    /// The short code RomM shows under Profile → Client API Tokens → Pair.
    PairCode(&'a str),
    /// A token made in RomM.
    Token(&'a str),
}

impl<'a> Credentials<'a> {
    /// Picks what was given. A blank entry counts as not given, so a form can send both fields.
    pub fn from_inputs(code: Option<&'a str>, token: Option<&'a str>) -> Result<Credentials<'a>> {
        let given = |v: Option<&'a str>| v.filter(|s| !s.trim().is_empty());
        match (given(code), given(token)) {
            (Some(code), None) => Ok(Credentials::PairCode(code)),
            (None, Some(token)) => Ok(Credentials::Token(token)),
            _ => Err(Error::msg("Give either a pairing code or a token.")),
        }
    }
}

/// A signed-in connection.
pub struct Connected {
    pub client: RommClient,
    /// The token to keep (`store_token`).
    pub token: String,
    /// What the token lacks that is needed. Only known for a token made by pairing, which says what
    /// it may do; empty for one that was typed in.
    pub missing_scopes: Vec<&'static str>,
}

impl Connected {
    /// What to tell the user about a token that lacks scopes, if it does.
    pub fn scope_warning(&self) -> Option<String> {
        if self.missing_scopes.is_empty() {
            return None;
        }
        Some(format!(
            "This token is missing scopes that are needed ({}). Create a new token with them in RomM → Profile → Client API Tokens.",
            self.missing_scopes.join(", ")
        ))
    }
}

/// Signs in to `server`: trades the pairing code, if that is what was given, then proves the token
/// works by asking for the platforms. Errors are worded for the user.
pub async fn connect(server: &str, credentials: Credentials<'_>, insecure: bool) -> Result<Connected> {
    let (token, scopes) = match credentials {
        Credentials::PairCode(code) => {
            let t = RommClient::exchange_pair_code(server, code, insecure).await.map_err(|e| {
                Error::msg(match e.status() {
                    Some(404) => "That pairing code wasn't recognised. Codes are single-use and expire after 5 minutes, so create a fresh one in RomM (Profile → Client API Tokens → Pair).".to_string(),
                    Some(401) | Some(403) => format!("The server refused the request: {e}"),
                    _ => format!("Couldn't reach the server or pair with it: {e}"),
                })
            })?;
            (t.raw_token, Some(t.scopes))
        }
        Credentials::Token(t) => (t.trim().to_string(), None),
    };
    let client = RommClient::new(server, Some(token.clone()), insecure)?;
    client.platforms().await.map_err(|e| Error::msg(format!("The server accepted the token but listing platforms failed: {e}")))?;
    let missing_scopes = scopes.map(|s| missing_scopes(&s)).unwrap_or_default();
    Ok(Connected { client, token, missing_scopes })
}
