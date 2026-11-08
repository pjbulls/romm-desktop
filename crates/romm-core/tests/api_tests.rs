use std::collections::HashMap;

use romm_core::api::RommClient;
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn rom_json(id: i64) -> Value {
    json!({
        "id": id, "name": format!("Game {id}"), "fs_name": format!("game{id}.sfc"), "fs_name_no_ext": format!("game{id}"),
        "fs_size_bytes": 10, "platform_id": 3, "platform_slug": "snes", "platform_fs_slug": "snes",
        "has_multiple_files": false, "files": [],
        "some_future_field": {"ignored": true}
    })
}

#[tokio::test]
async fn pair_code_exchange_needs_no_auth_and_returns_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/client-tokens/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "desktop", "scopes": ["roms.read", "assets.write"], "user_id": 1, "raw_token": "rmm_abc", "device_id": null
        })))
        .mount(&server)
        .await;
    let t = RommClient::exchange_pair_code(&server.uri(), " 12345678 ", false).await.unwrap();
    assert_eq!(t.raw_token, "rmm_abc");
    assert_eq!(t.scopes, ["roms.read", "assets.write"]);
    let req = &server.received_requests().await.unwrap()[0];
    assert!(req.headers.get("authorization").is_none());
    assert_eq!(req.body_json::<Value>().unwrap(), json!({"code": "12345678"}));
}

#[tokio::test]
async fn bad_pair_code_surfaces_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/client-tokens/exchange"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "Invalid or expired code"})))
        .mount(&server)
        .await;
    let err = RommClient::exchange_pair_code(&server.uri(), "00000000", false).await.unwrap_err();
    assert_eq!(err.status(), Some(404));
    assert!(err.to_string().contains("Invalid or expired code"));
}

#[tokio::test]
async fn platforms_include_firmware_and_tolerate_extra_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/platforms"))
        .and(header("authorization", "Bearer rmm_t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 5, "slug": "psx", "fs_slug": "psx", "name": "PlayStation", "display_name": "Sony PlayStation", "rom_count": 12,
            "igdb_id": 7, "future": 1,
            "firmware": [{"id": 9, "platform_id": 5, "file_name": "scph5501.bin", "file_size_bytes": 524288,
                          "md5_hash": "490f666e1afb15b7362b406ed1cea246", "is_verified": true, "missing_from_fs": false}]
        }])))
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let ps = c.platforms().await.unwrap();
    assert_eq!(ps[0].title(), "Sony PlayStation");
    assert_eq!(ps[0].firmware[0].file_name, "scph5501.bin");
    assert!(ps[0].firmware[0].is_verified);
}

#[tokio::test]
async fn roms_query_and_paging() {
    let server = MockServer::start().await;
    let first: Vec<Value> = (1..=500).map(rom_json).collect();
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .and(query_param("offset", "0"))
        .and(query_param("platform_ids", "3"))
        .and(query_param("with_char_index", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": first, "total": 503, "limit": 500, "offset": 0})))
        .mount(&server)
        .await;
    let rest: Vec<Value> = (501..=503).map(rom_json).collect();
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .and(query_param("offset", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": rest, "total": 503, "limit": 500, "offset": 500})))
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let all = c.all_roms(Some(3), None).await.unwrap();
    assert_eq!(all.len(), 503);
    assert_eq!(all[502].title(), "Game 503");
}

#[tokio::test]
async fn search_term_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .and(query_param("search_term", "zelda"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [], "total": 0, "limit": 50, "offset": 0})))
        .expect(1)
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    assert!(c.roms(None, Some("zelda"), 50, 0).await.unwrap().items.is_empty());
}

#[tokio::test]
async fn library_fetches_every_page_grouped_without_files_and_without_duplicates() {
    let server = MockServer::start().await;
    let first: Vec<Value> = (1..=500).map(rom_json).collect();
    // Page two repeats id 500 (the library changed between requests) and adds two more.
    let second: Vec<Value> = vec![rom_json(500), rom_json(501), rom_json(502)];
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .and(query_param("offset", "0"))
        .and(query_param("group_by_meta_id", "true"))
        .and(query_param("with_files", "false"))
        .and(query_param("order_by", "name"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": first, "total": 503, "limit": 500, "offset": 0})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .and(query_param("offset", "500"))
        .and(query_param("group_by_meta_id", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": second, "total": null, "limit": 500, "offset": 500})))
        .expect(1)
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let mut seen = Vec::new();
    let all = c.library(&mut |loaded, total| seen.push((loaded, total))).await.unwrap();

    assert_eq!(all.len(), 502, "the repeated row is dropped");
    assert_eq!(all.iter().filter(|r| r.id == 500).count(), 1);
    assert_eq!(seen, [(500, Some(503)), (502, Some(503))], "progress after each page, total known from the first");

    let reqs = server.received_requests().await.unwrap();
    let has_total_param = |i: usize| reqs[i].url.query_pairs().any(|(k, _)| k == "with_total");
    assert!(!has_total_param(0), "the first page asks for the count");
    assert!(reqs[1].url.query_pairs().any(|(k, v)| k == "with_total" && v == "false"), "later pages don't");
    assert!(reqs.iter().all(|r| !r.url.query_pairs().any(|(k, _)| k == "platform_ids" || k == "search_term")), "the whole library, filtered locally");
}

#[tokio::test]
async fn library_of_a_single_short_page_is_one_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [rom_json(1), rom_json(2)], "total": 2, "limit": 500, "offset": 0})))
        .expect(1)
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    assert_eq!(c.library(&mut |_, _| {}).await.unwrap().len(), 2);
}

#[tokio::test]
async fn an_empty_library_is_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [], "total": 0, "limit": 500, "offset": 0})))
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    assert!(c.library(&mut |_, _| {}).await.unwrap().is_empty());
}

#[tokio::test]
async fn sibling_roms_are_read_and_absent_ones_default_to_none() {
    let server = MockServer::start().await;
    let mut with = rom_json(1);
    with["sibling_roms"] = json!([
        {"id": 2, "name": "Game 1", "fs_name_no_tags": "game1", "fs_name_no_ext": "game1 (Japan)", "is_main_sibling": false, "sort_comparator": "game 1"},
        {"id": 3, "name": "Game 1", "fs_name_no_tags": "game1", "fs_name_no_ext": "game1 (Europe)", "is_main_sibling": true}
    ]);
    Mock::given(method("GET"))
        .and(path("/api/roms"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [with, rom_json(9)], "total": 2, "limit": 500, "offset": 0})))
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let all = c.library(&mut |_, _| {}).await.unwrap();
    let ids: Vec<i64> = all[0].sibling_roms.iter().map(|s| s.id).collect();
    assert_eq!(ids, [2, 3]);
    assert!(all[0].sibling_roms[1].is_main_sibling);
    assert_eq!(all[0].sibling_roms[0].fs_name_no_ext, "game1 (Japan)");
    assert!(all[1].sibling_roms.is_empty(), "an older server that sends none still parses");
}

#[tokio::test]
async fn rom_sizes_reads_each_rom_once_in_order_and_skips_the_ones_that_fail() {
    let server = MockServer::start().await;
    let mut big = rom_json(3);
    big["fs_size_bytes"] = json!(300);
    big["has_multiple_files"] = json!(true);
    let mut small = rom_json(1);
    small["fs_size_bytes"] = json!(100);
    for (id, body) in [(1, small), (3, big)] {
        Mock::given(method("GET")).and(path(format!("/api/roms/{id}"))).respond_with(ResponseTemplate::new(200).set_body_json(body)).expect(1).mount(&server).await;
    }
    Mock::given(method("GET")).and(path("/api/roms/2")).respond_with(ResponseTemplate::new(404)).expect(1).mount(&server).await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();

    // 3 comes before 1, and 1 is asked for twice, 2 doesn't exist.
    let got = c.rom_sizes(&[3, 2, 1, 3, 1]).await;
    assert_eq!(
        got,
        [
            romm_core::api::types::RomSize { id: 3, bytes: 300, multi_file: true },
            romm_core::api::types::RomSize { id: 1, bytes: 100, multi_file: false },
        ]
    );
}

#[tokio::test]
async fn rom_sizes_of_nothing_asks_for_nothing() {
    let server = MockServer::start().await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    assert!(c.rom_sizes(&[]).await.is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn rom_sizes_never_asks_for_more_than_a_bounded_number() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(rom_json(1))).mount(&server).await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let ids: Vec<i64> = (1..=100).collect();
    assert_eq!(c.rom_sizes(&ids).await.len(), 32);
    assert_eq!(server.received_requests().await.unwrap().len(), 32);
}

#[tokio::test]
async fn download_streams_to_file_and_leaves_no_part_file_on_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms/1/content/game1.sfc"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 100_000]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/roms/2/content/game2.sfc"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let t = tempfile::tempdir().unwrap();

    let rom1: romm_core::api::types::Rom = serde_json::from_value(rom_json(1)).unwrap();
    let dest = t.path().join("snes/game1.sfc");
    let mut last = (0u64, None);
    c.download_rom(&rom1, &dest, &mut |d, tot| last = (d, tot)).await.unwrap();
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 100_000);
    assert_eq!(last.0, 100_000);
    assert!(!t.path().join("snes/game1.sfc.part").exists());

    let rom2: romm_core::api::types::Rom = serde_json::from_value(rom_json(2)).unwrap();
    let dest2 = t.path().join("snes/game2.sfc");
    let err = c.download_rom(&rom2, &dest2, &mut |_, _| {}).await.unwrap_err();
    assert_eq!(err.status(), Some(404));
    assert!(!dest2.exists() && !t.path().join("snes/game2.sfc.part").exists());
}

#[tokio::test]
async fn multi_file_download_requests_specific_file_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms/9/content/Final%20Fantasy%20VII"))
        .and(query_param("file_ids", "11,12"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"zip".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let rom: romm_core::api::types::Rom = serde_json::from_value(json!({
        "id": 9, "name": "FF7", "fs_name": "Final Fantasy VII", "fs_name_no_ext": "Final Fantasy VII", "platform_id": 5,
        "platform_slug": "psx", "platform_fs_slug": "psx", "has_multiple_files": true,
        "files": [{"id": 11, "rom_id": 9, "file_name": "d1.cue"}, {"id": 12, "rom_id": 9, "file_name": "d2.cue"}]
    })).unwrap();
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let t = tempfile::tempdir().unwrap();
    c.download_rom(&rom, &t.path().join("ff7.zip"), &mut |_, _| {}).await.unwrap();
}

#[tokio::test]
async fn device_registration_is_sent_as_documented() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/devices"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"device_id": "uuid-1", "name": "Desk PC", "created_at": "2026-01-01T00:00:00Z"})))
        .mount(&server)
        .await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let d = c
        .register_device(&romm_core::api::types::DeviceRegistration {
            name: "Desk PC".into(),
            platform: "linux".into(),
            client: "romm-desktop".into(),
            client_version: "0.1.0".into(),
            hostname: Some("desk".into()),
            sync_mode: "api".into(),
        })
        .await
        .unwrap();
    assert_eq!(d.device_id, "uuid-1");
    let body: HashMap<String, Value> = server.received_requests().await.unwrap()[0].body_json().unwrap();
    assert_eq!(body["sync_mode"], "api");
    assert_eq!(body["client"], "romm-desktop");
}

// RomM's auth updates the token's "last seen" rows on every request; two requests arriving
// together race on MariaDB with snapshot isolation and one gets a 500. API calls must therefore
// take turns, including across clones of the client (the app hands a clone to every command).
// Each response is delayed 300 ms, so serial calls take >= 600 ms and parallel ones ~300 ms.
async fn slow_empty_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/platforms"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])).set_delay(std::time::Duration::from_millis(300)))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn api_calls_take_turns() {
    let server = slow_empty_server().await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let started = std::time::Instant::now();
    let (a, b) = tokio::join!(c.platforms(), c.platforms());
    a.unwrap();
    b.unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(580), "requests overlapped: {:?}", started.elapsed());
}

#[tokio::test]
async fn clones_share_the_same_turn_order() {
    let server = slow_empty_server().await;
    let c = RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap();
    let other = c.clone();
    let started = std::time::Instant::now();
    let (a, b) = tokio::join!(c.platforms(), other.platforms());
    a.unwrap();
    b.unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(580), "clones overlapped: {:?}", started.elapsed());
}

// ---- RomM's auth step can fail a request that did nothing wrong ---------------------------------
//
// With MariaDB's snapshot isolation, two requests on one client token that arrive together can race
// to update `client_tokens.last_used_at`; the loser gets an HTTP 500 ("Record has changed since last
// read in table 'client_tokens'") before its handler has run at all. Calls that are safe to make
// twice are therefore repeated; nothing that could be stored twice ever is.

fn authed(server: &MockServer) -> RommClient {
    RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap()
}

fn negotiate_req() -> romm_core::api::types::NegotiateRequest {
    romm_core::api::types::NegotiateRequest { device_id: "dev".into(), saves: vec![], rom_ids: vec![969], restore_unlisted: false, emulators: None }
}

/// The 500 RomM sent (its body is just this text, so nothing else tells the cause apart).
fn glitch() -> ResponseTemplate {
    ResponseTemplate::new(500).set_body_string("Internal Server Error")
}

async fn count(server: &MockServer, m: &str, p: &str) -> usize {
    server.received_requests().await.unwrap().iter().filter(|r| r.method.as_str() == m && r.url.path() == p).count()
}

#[tokio::test]
async fn a_negotiate_that_hits_the_auth_race_is_simply_made_again() {
    let server = MockServer::start().await;
    // What the log showed: a 500 at 20:45:18, though the same call had worked at 20:43:35.
    Mock::given(method("POST")).and(path("/api/sync/negotiate")).respond_with(glitch()).up_to_n_times(1).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 397, "operations": []})))
        .mount(&server)
        .await;

    let r = authed(&server).negotiate(&negotiate_req()).await.expect("the second attempt succeeds, so the sync does");
    assert_eq!(r.session_id, 397);
    assert_eq!(count(&server, "POST", "/api/sync/negotiate").await, 2);
}

#[tokio::test]
async fn reads_and_the_other_sync_calls_are_retried_too() {
    let server = MockServer::start().await;
    for (m, p, body) in [
        ("GET", "/api/users/me", json!({"id": 1, "username": "u", "role": "admin", "ra_user_id": null})),
        ("POST", "/api/devices", json!({"id": "d", "name": "n", "client": "c"})),
        ("POST", "/api/sync/sessions/5/complete", json!({})),
    ] {
        Mock::given(method(m)).and(path(p)).respond_with(glitch()).up_to_n_times(1).mount(&server).await;
        Mock::given(method(m)).and(path(p)).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&server).await;
    }
    let c = authed(&server);
    let _ = c.me().await; // shapes vary between RomM versions; what matters is that a second request was made
    assert_eq!(count(&server, "GET", "/api/users/me").await, 2, "a read is retried");
    c.complete_session(5, &romm_core::api::types::CompleteRequest { operations_completed: 0, operations_failed: 0, play_sessions: vec![] }).await.unwrap();
    assert_eq!(count(&server, "POST", "/api/sync/sessions/5/complete").await, 2, "completing the session is retried");
}

#[tokio::test]
async fn a_server_that_keeps_failing_is_given_up_on_after_three_tries() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/sync/negotiate")).respond_with(glitch()).mount(&server).await;
    let started = std::time::Instant::now();
    let e = authed(&server).negotiate(&negotiate_req()).await.unwrap_err();
    assert_eq!(e.status(), Some(500), "the real status still reaches the caller: {e}");
    assert_eq!(count(&server, "POST", "/api/sync/negotiate").await, 3, "one try and two retries, no more");
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "and it does not stall the app");
}

#[tokio::test]
async fn an_answer_that_is_not_a_glitch_is_never_retried() {
    for status in [400u16, 401, 403, 404, 409, 422] {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/api/sync/negotiate")).respond_with(ResponseTemplate::new(status).set_body_string("no")).mount(&server).await;
        let e = authed(&server).negotiate(&negotiate_req()).await.unwrap_err();
        assert_eq!(e.status(), Some(status));
        assert_eq!(count(&server, "POST", "/api/sync/negotiate").await, 1, "{status} is an answer, not a glitch");
    }
}

#[tokio::test]
async fn an_upload_is_never_repeated_because_it_could_be_stored_twice() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(glitch()).mount(&server).await;
    Mock::given(method("PUT")).and(path("/api/saves/9")).respond_with(glitch()).mount(&server).await;
    let c = authed(&server);
    let p = romm_core::api::types::UploadParams { rom_id: 1, file_name: "a.zip".into(), ..Default::default() };
    assert_eq!(c.upload_save(&p, vec![1, 2, 3]).await.unwrap_err().status(), Some(500));
    assert_eq!(c.replace_save(9, &p, vec![1, 2, 3]).await.unwrap_err().status(), Some(500));
    assert_eq!(count(&server, "POST", "/api/saves").await, 1, "one attempt: the first might have been stored");
    assert_eq!(count(&server, "PUT", "/api/saves/9").await, 1);
}

#[tokio::test]
async fn a_slow_retry_never_overlaps_the_next_request() {
    // The turn-taking lock is held per attempt, not across the wait, so a retry does not freeze other
    // calls, and two calls never reach RomM's auth together.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(glitch()).up_to_n_times(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&server).await;
    let c = authed(&server);
    let (a, b) = tokio::join!(c.heartbeat(), c.heartbeat());
    assert!(a.is_ok() && b.is_ok(), "{a:?} {b:?}");
}

// ---- the desktop app and the CLI share one token, in two processes -------------------------------

/// Records when each request reaches the server, and answers slowly, so that two requests that
/// were sent together show up as arriving together.
#[derive(Clone, Default)]
struct Arrivals(std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>);

impl wiremock::Respond for Arrivals {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        self.0.lock().unwrap().push(std::time::Instant::now());
        ResponseTemplate::new(200).set_body_json(json!({})).set_delay(std::time::Duration::from_millis(300))
    }
}

async fn two_programs_call_at_once(server: &MockServer, lock: Option<&std::path::Path>) -> std::time::Duration {
    let arrivals = Arrivals::default();
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(arrivals.clone()).mount(server).await;
    // Two separate clients: each has its own in-process lock, exactly as the app and the CLI do.
    let make = || {
        let c = authed(server);
        match lock {
            Some(p) => c.with_process_lock(p),
            None => c,
        }
    };
    let (a, b) = (make(), make());
    let (ra, rb) = tokio::join!(a.heartbeat(), b.heartbeat());
    assert!(ra.is_ok() && rb.is_ok(), "{ra:?} {rb:?}");
    let t = arrivals.0.lock().unwrap().clone();
    assert_eq!(t.len(), 2);
    t[1].duration_since(t[0])
}

#[tokio::test]
async fn two_programs_sharing_a_lock_file_reach_the_server_one_after_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let gap = two_programs_call_at_once(&MockServer::start().await, Some(&dir.path().join("api.lock"))).await;
    assert!(gap >= std::time::Duration::from_millis(250), "the second waited for the first's response: {gap:?}");
}

#[tokio::test]
async fn without_a_shared_lock_two_programs_do_arrive_together() {
    // The control: this is the collision that made RomM answer 500, and what the test above rules out.
    let gap = two_programs_call_at_once(&MockServer::start().await, None).await;
    assert!(gap < std::time::Duration::from_millis(150), "{gap:?}");
}

#[tokio::test]
async fn a_lock_file_that_cannot_be_used_never_holds_a_request_up() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("file");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&server).await;
    let started = std::time::Instant::now();
    // A "folder" that is really a file, so the lock file cannot be created.
    authed(&server).with_process_lock(not_a_dir.join("api.lock")).heartbeat().await.expect("the request goes ahead without the lock");
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
}

#[tokio::test]
async fn a_lock_that_is_never_let_go_only_delays_a_request_a_little() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("api.lock");
    // Some other program holds it for good (it hung, say).
    let held = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&lock).unwrap();
    held.lock().unwrap();

    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&server).await;
    let started = std::time::Instant::now();
    authed(&server).with_process_lock(&lock).with_process_lock_wait(std::time::Duration::from_millis(300)).heartbeat().await.expect("goes ahead after waiting its turn");
    let took = started.elapsed();
    assert!(took >= std::time::Duration::from_millis(280) && took < std::time::Duration::from_secs(3), "{took:?}");
    drop(held);
}

#[tokio::test]
async fn the_lock_is_free_again_as_soon_as_a_request_has_its_headers() {
    // Not held across the body: a second request is not kept waiting for a long download.
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("api.lock");
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&server).await;
    let c = authed(&server).with_process_lock(&lock);
    c.heartbeat().await.unwrap();
    let probe = std::fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();
    probe.try_lock().expect("nothing is still holding the lock after the call returns");
}

// ---- two real processes ------------------------------------------------------------------------

/// Not a test of its own: the body the child processes below run. Without the variables it does nothing.
#[tokio::test]
async fn child_process_makes_a_call() {
    let (Ok(url), Ok(start_at)) = (std::env::var("ROMM_TEST_URL"), std::env::var("ROMM_TEST_START_AT_MS")) else { return };
    let client = RommClient::new(&url, Some("rmm_t".into()), false).unwrap();
    let client = match std::env::var("ROMM_TEST_LOCK") {
        Ok(p) if !p.is_empty() => client.with_process_lock(p),
        _ => client,
    };
    // Everyone starts together, however long each process took to come up.
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(start_at.parse().unwrap());
    if let Ok(wait) = at.duration_since(std::time::SystemTime::now()) {
        tokio::time::sleep(wait).await;
    }
    client.heartbeat().await.expect("the call succeeds");
}

async fn two_real_processes_call_at_once(lock: Option<&std::path::Path>) -> std::time::Duration {
    let server = MockServer::start().await;
    let arrivals = Arrivals::default();
    Mock::given(method("GET")).and(path("/api/heartbeat")).respond_with(arrivals.clone()).mount(&server).await;
    let start_at = (std::time::SystemTime::now() + std::time::Duration::from_millis(1500)).duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let me = std::env::current_exe().unwrap();
    let spawn = || {
        let mut c = std::process::Command::new(&me);
        c.args(["--exact", "child_process_makes_a_call", "--test-threads=1"])
            .env("ROMM_TEST_URL", server.uri())
            .env("ROMM_TEST_START_AT_MS", start_at.to_string())
            .env("ROMM_TEST_LOCK", lock.map(|p| p.as_os_str().to_owned()).unwrap_or_default())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        c.spawn().expect("a child process starts")
    };
    let (mut a, mut b) = (spawn(), spawn());
    let (sa, sb) = tokio::task::spawn_blocking(move || (a.wait().unwrap(), b.wait().unwrap())).await.unwrap();
    assert!(sa.success() && sb.success(), "both processes made their call: {sa:?} {sb:?}");
    let t = arrivals.0.lock().unwrap().clone();
    assert_eq!(t.len(), 2, "each process sent exactly one request");
    t[1].duration_since(t[0])
}

#[tokio::test]
async fn two_real_processes_sharing_the_lock_file_take_turns() {
    let dir = tempfile::tempdir().unwrap();
    let gap = two_real_processes_call_at_once(Some(&dir.path().join("api.lock"))).await;
    assert!(gap >= std::time::Duration::from_millis(250), "the second process waited for the first's response: {gap:?}");
}

#[tokio::test]
async fn two_real_processes_without_it_collide_which_is_what_romm_choked_on() {
    let gap = two_real_processes_call_at_once(None).await;
    assert!(gap < std::time::Duration::from_millis(200), "{gap:?}");
}
