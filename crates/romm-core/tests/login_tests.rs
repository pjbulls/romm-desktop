//! What every front end needs to get logged in: the device it registers, and the scopes its token needs.

use romm_core::api::{RommClient, CLIENT_SLUG, REQUIRED_SCOPES};
use romm_core::config::{default_device_name, register_device};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
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
