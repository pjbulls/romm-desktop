//! What every front end needs to get logged in: signing in, the device it registers, and the scopes
//! its token needs.

use romm_core::api::{RommClient, CLIENT_SLUG, REQUIRED_SCOPES};
use romm_core::config::{default_device_name, register_device};
use romm_core::login::{connect, Credentials};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(s: &MockServer) -> RommClient {
    RommClient::new(&s.uri(), Some("rmm_t".into()), false).unwrap()
}

async fn registered_body(server: &MockServer) -> Value {
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "one registration, no retries");
    serde_json::from_slice(&reqs[0].body).unwrap()
}

#[tokio::test]
async fn the_device_is_registered_as_this_client_with_the_name_the_user_chose() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/devices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"device_id": "dev-123", "name": "Steam Deck"}))).mount(&server).await;

    let id = register_device(&client(&server), Some("Steam Deck")).await.unwrap();

    assert_eq!(id, "dev-123");
    let body = registered_body(&server).await;
    assert_eq!(body["name"], "Steam Deck");
    assert_eq!(body["client"], CLIENT_SLUG);
    assert_eq!(body["client_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["platform"], std::env::consts::OS);
    assert_eq!(body["sync_mode"], "api");
}

#[tokio::test]
async fn with_no_name_chosen_the_device_is_named_after_the_machine() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/devices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"device_id": "dev-9"}))).mount(&server).await;

    register_device(&client(&server), None).await.unwrap();

    assert_eq!(registered_body(&server).await["name"], default_device_name().as_str());
}

#[tokio::test]
async fn a_refused_registration_points_at_the_scope_that_is_probably_missing() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/devices")).respond_with(ResponseTemplate::new(403)).mount(&server).await;

    let err = register_device(&client(&server), None).await.unwrap_err().to_string();

    assert!(err.contains("devices.write"), "{err}");
}

/// The scopes are written out for people in two other places. They are prose, so nothing else
/// would notice one drifting from what the code asks of the token.
#[test]
fn the_scopes_the_docs_and_the_login_form_list_are_the_ones_required() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");
    let login = std::fs::read_to_string(root.join("app/src/components/Login.tsx")).expect("Login.tsx");
    assert!(readme.contains(&format!("`{}`", REQUIRED_SCOPES.join(" "))), "README.md lists different scopes than REQUIRED_SCOPES");
    assert!(login.contains(&format!("{}.", REQUIRED_SCOPES.join(", "))), "Login.tsx lists different scopes than REQUIRED_SCOPES");
}

// ---- signing in (`login::connect`)

fn exchange_ok(scopes: &[&str]) -> Mock {
    Mock::given(method("POST"))
        .and(path("/api/client-tokens/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "desktop", "scopes": scopes, "user_id": 1, "raw_token": "rmm_abc", "device_id": null
        })))
}

fn platforms_ok(token: &str) -> Mock {
    Mock::given(method("GET")).and(path("/api/platforms")).and(header("authorization", format!("Bearer {token}").as_str())).respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
}

#[tokio::test]
async fn pairing_trades_the_code_proves_the_token_and_says_what_it_lacks() {
    let server = MockServer::start().await;
    exchange_ok(&["roms.read", "assets.write"]).mount(&server).await;
    platforms_ok("rmm_abc").mount(&server).await;

    let c = connect(&server.uri(), Credentials::PairCode("1234-5678"), false).await.unwrap();

    assert_eq!(c.token, "rmm_abc");
    assert!(c.missing_scopes.contains(&"platforms.read") && !c.missing_scopes.contains(&"roms.read"), "{:?}", c.missing_scopes);
    let warning = c.scope_warning().expect("a token that lacks scopes is warned about");
    assert!(warning.contains("platforms.read") && warning.contains("Client API Tokens"), "{warning}");
    // The code goes to the server the way RomM shows it to be typed, without the dash.
    let sent = server.received_requests().await.unwrap()[0].body_json::<Value>().unwrap();
    assert_eq!(sent, json!({"code": "12345678"}));
}

#[tokio::test]
async fn a_token_with_every_scope_gets_no_warning() {
    let server = MockServer::start().await;
    exchange_ok(REQUIRED_SCOPES).mount(&server).await;
    platforms_ok("rmm_abc").mount(&server).await;
    let c = connect(&server.uri(), Credentials::PairCode("12345678"), false).await.unwrap();
    assert!(c.missing_scopes.is_empty() && c.scope_warning().is_none());
}

#[tokio::test]
async fn a_typed_token_is_trimmed_and_its_scopes_are_not_known() {
    let server = MockServer::start().await;
    platforms_ok("rmm_typed").mount(&server).await;
    let c = connect(&server.uri(), Credentials::Token("  rmm_typed \n"), false).await.unwrap();
    assert_eq!(c.token, "rmm_typed");
    assert!(c.scope_warning().is_none(), "nothing is known about a typed token's scopes");
}

#[tokio::test]
async fn an_unknown_pairing_code_is_explained() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/client-tokens/exchange")).respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "Invalid or expired code"}))).mount(&server).await;
    let err = connect(&server.uri(), Credentials::PairCode("00000000"), false).await.err().expect("should fail");
    assert!(err.to_string().contains("wasn't recognised") && err.to_string().contains("5 minutes"), "{err}");
}

#[tokio::test]
async fn a_refused_request_and_an_unreachable_server_are_told_apart() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/client-tokens/exchange")).respond_with(ResponseTemplate::new(403)).mount(&server).await;
    let err = connect(&server.uri(), Credentials::PairCode("12345678"), false).await.err().unwrap();
    assert!(err.to_string().starts_with("The server refused the request"), "{err}");

    // Nothing listens on the discard port (the other tests use it for a server that isn't there).
    let err = connect("http://127.0.0.1:9", Credentials::PairCode("12345678"), false).await.err().unwrap();
    assert!(err.to_string().starts_with("Couldn't reach the server"), "{err}");
}

#[tokio::test]
async fn a_token_the_server_does_not_accept_is_reported_as_such() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/platforms")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    let err = connect(&server.uri(), Credentials::Token("rmm_wrong"), false).await.err().unwrap();
    assert!(err.to_string().starts_with("The server accepted the token but listing platforms failed"), "{err}");
}

#[test]
fn exactly_one_of_a_code_and_a_token_is_needed_and_blank_means_not_given() {
    assert_eq!(Credentials::from_inputs(Some("1234"), None).unwrap(), Credentials::PairCode("1234"));
    assert_eq!(Credentials::from_inputs(Some("  "), Some("rmm_t")).unwrap(), Credentials::Token("rmm_t"));
    assert_eq!(Credentials::from_inputs(Some(""), Some("rmm_t")).unwrap(), Credentials::Token("rmm_t"));
    assert!(Credentials::from_inputs(None, None).is_err());
    assert!(Credentials::from_inputs(Some("1234"), Some("rmm_t")).is_err());
    assert!(Credentials::from_inputs(Some(" "), Some(" ")).is_err());
}
