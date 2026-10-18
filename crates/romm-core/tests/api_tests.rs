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
