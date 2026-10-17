//! End-to-end tests of the sync engine and play sessions against a mock RomM server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use romm_core::api::RommClient;
use romm_core::paths::{Os, PathCtx};
use romm_core::profiles;
use romm_core::retroarch;
use romm_core::saves;
use romm_core::state::{save_key, LocalRom, State};
use romm_core::sync::{ConflictPolicy, EmuEnv, Syncer};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const DEVICE: &str = "dev-1";

fn retroarch_env(root: &Path) -> EmuEnv {
    EmuEnv {
        profile: profiles::get("retroarch").unwrap(),
        data_root: root.to_path_buf(),
        retroarch: Some(retroarch::layout(root, Os::Linux, &PathCtx::default())),
    }
}

fn diff_env(id: &str, root: &Path) -> EmuEnv {
    EmuEnv { profile: profiles::get(id).unwrap(), data_root: root.to_path_buf(), retroarch: None }
}

fn rom(stem: &str, platform: &str) -> LocalRom {
    LocalRom {
        rom_id: 7,
        name: stem.into(),
        platform_slug: platform.into(),
        path: PathBuf::from(format!("/roms/{stem}")),
        launch_path: PathBuf::from(format!("/roms/{stem}.rom")),
        stem: stem.into(),
        ..Default::default()
    }
}

fn save_json(id: i64, name: &str) -> Value {
    json!({"id": id, "rom_id": 7, "file_name": name, "file_size_bytes": 1, "emulator": "x", "slot": null, "content_hash": null})
}

fn op(action: &str, save_id: Option<i64>, name: &str) -> Value {
    json!({"action": action, "rom_id": 7, "save_id": save_id, "file_name": name, "slot": "autosave", "emulator": null})
}

async fn mock_negotiate(server: &MockServer, ops: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 42, "operations": ops})))
        .mount(server)
        .await;
}

async fn mock_complete(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/sync/sessions/\d+/complete$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}

async fn reqs(server: &MockServer, m: &str, p: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == m && r.url.path() == p)
        .collect()
}

fn query(r: &Request) -> HashMap<String, String> {
    r.url.query_pairs().into_owned().collect()
}

fn client(server: &MockServer) -> RommClient {
    RommClient::new(&server.uri(), Some("rmm_test".into()), false).unwrap()
}

fn syncer<'a>(c: &'a RommClient, st: &'a mut State, bak: &Path, policy: ConflictPolicy) -> Syncer<'a> {
    Syncer { client: c, device_id: DEVICE, state: st, backup_dir: bak.to_path_buf(), policy }
}

fn find_file_containing(dir: &Path, needle: &[u8]) -> bool {
    walk(dir).iter().any(|p| std::fs::read(p).map(|b| b == needle).unwrap_or(false))
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

// ----------------------------------------------------------------- RetroArch (raw files)

#[tokio::test]
async fn fresh_device_downloads_server_save() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Zelda.srm")]).await;
    Mock::given(method("GET"))
        .and(path("/api/saves/99/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SERVERSAVE".to_vec()))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"));
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();

    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"SERVERSAVE");
    assert_eq!(rep.downloaded, ["Zelda.srm"]);
    assert!(rep.is_clean());

    let neg = &reqs(&server, "POST", "/api/sync/negotiate").await[0];
    let body: Value = neg.body_json().unwrap();
    assert_eq!(body["device_id"], DEVICE);
    assert_eq!(body["rom_ids"], json!([7]));
    assert_eq!(body["emulators"], json!(["retroarch"]));
    assert_eq!(body["saves"], json!([]));
    assert_eq!(neg.headers.get("authorization").unwrap().to_str().unwrap(), "Bearer rmm_test");

    let done: Value = reqs(&server, "POST", "/api/sync/sessions/42/complete").await[0].body_json().unwrap();
    assert_eq!(done["operations_completed"], 1);
    assert_eq!(done["operations_failed"], 0);
}

#[tokio::test]
async fn local_save_is_negotiated_and_uploaded() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Zelda.srm")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(save_json(11, "Zelda.srm")))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"));
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();

    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();
    assert_eq!(rep.uploaded, ["Zelda.srm"]);

    let md5 = saves::md5_bytes(b"LOCAL");
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["content_hash"], md5.as_str());
    assert_eq!(neg["saves"][0]["slot"], "autosave");
    assert_eq!(neg["saves"][0]["emulator"], "retroarch");
    assert_eq!(neg["saves"][0]["file_name"], "Zelda.srm");

    let up = &reqs(&server, "POST", "/api/saves").await[0];
    let q = query(up);
    assert_eq!(q["rom_id"], "7");
    assert_eq!(q["slot"], "autosave");
    assert_eq!(q["emulator"], "retroarch");
    assert_eq!(q["device_id"], DEVICE);
    assert_eq!(q["session_id"], "42");
    assert_eq!(q["content_hash"], md5);
    assert_eq!(q["overwrite"], "false");
    let body = String::from_utf8_lossy(&up.body);
    assert!(body.contains("name=\"saveFile\"") && body.contains("LOCAL"), "{body}");

    let rec = &st.saves[&save_key(7, "retroarch")];
    assert_eq!(rec.server_save_id, Some(11));
    assert_eq!(rec.last_hash.as_deref(), Some(md5.as_str()));
}

#[tokio::test]
async fn conflict_keep_both_archives_local_then_adopts_server() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Zelda.srm")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(save_json(12, "archived")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/saves/5/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SERVER".to_vec()))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"));
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL-PROGRESS").unwrap();

    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();

    // The local copy went to the server as an archival (slot-less) save...
    let up = &reqs(&server, "POST", "/api/saves").await[0];
    assert!(!query(up).contains_key("slot"), "archival saves must not claim the slot");
    let body = String::from_utf8_lossy(&up.body);
    assert!(body.contains("conflict-") && body.contains("LOCAL-PROGRESS"), "{body}");
    // ...and was also backed up on disk before being replaced by the server's.
    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"SERVER");
    assert!(find_file_containing(&bak, b"LOCAL-PROGRESS"), "local save must be backed up");
    assert_eq!(rep.archived.len(), 1);
    assert_eq!(rep.downloaded, ["Zelda.srm"]);
    assert!(rep.is_clean());
}

#[tokio::test]
async fn conflict_ask_changes_nothing() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Zelda.srm")]).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"));
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();

    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::Ask)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"LOCAL");
    assert!(reqs(&server, "POST", "/api/saves").await.is_empty());
    assert!(reqs(&server, "GET", "/api/saves/5/content").await.is_empty());
}

#[tokio::test]
async fn conflict_device_wins_replaces_server_save() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Zelda.srm")]).await;
    Mock::given(method("PUT"))
        .and(path("/api/saves/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(save_json(5, "Zelda.srm")))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"));
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::DeviceWins)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();
    assert_eq!(rep.uploaded, ["Zelda.srm"]);
    let put = &reqs(&server, "PUT", "/api/saves/5").await[0];
    assert_eq!(query(put)["content_hash"], saves::md5_bytes(b"LOCAL"));
}

#[tokio::test]
async fn upload_409_is_reported_as_conflict_not_failure() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Zelda.srm")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(409).set_body_string("slot already filled"))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"));
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert!(rep.failed.is_empty());
    assert!(rep.uploaded.is_empty());
}

// ---------------------------------------------------------- shared-folder emulators (bundles)

#[tokio::test]
async fn bundle_download_restores_files_and_sets_baseline() {
    // Build the "server" bundle from another machine's Dolphin folder.
    let t = tempfile::tempdir().unwrap();
    let other = t.path().join("other");
    std::fs::create_dir_all(other.join("GC/USA")).unwrap();
    std::fs::write(other.join("GC/USA/Card A.raw"), b"CARD").unwrap();
    let bundle = saves::pack_bundle("dolphin", &other, &["GC/USA/Card A.raw".to_string()]).unwrap();

    let server = MockServer::start().await;
    // First negotiate offers the download; the second finds nothing to do.
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 1, "operations": [op("download", Some(3), "Melee.zip")]})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mock_negotiate(&server, vec![op("no_op", Some(3), "Melee.zip")]).await;
    Mock::given(method("GET"))
        .and(path("/api/saves/3/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bundle.clone()))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let me = t.path().join("me");
    let env = diff_env("dolphin", &me);
    let c = client(&server);
    let mut st = State::default();
    let r = rom("Melee", "ngc");

    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.downloaded, ["Melee.zip"]);
    assert_eq!(std::fs::read(me.join("GC/USA/Card A.raw")).unwrap(), b"CARD");
    assert_eq!(st.saves[&save_key(7, "dolphin")].files, ["GC/USA/Card A.raw"]);

    // Second sync: we must report the hash of what we downloaded, so the server sees "no change".
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.unchanged, 1);
    let second: Value = reqs(&server, "POST", "/api/sync/negotiate").await[1].body_json().unwrap();
    assert_eq!(second["saves"][0]["content_hash"], saves::md5_bytes(&bundle).as_str());
    assert_eq!(second["saves"][0]["file_name"], "Melee.zip");
    assert_eq!(second["saves"][0]["emulator"], "dolphin");
}

#[tokio::test]
async fn bundle_from_another_client_is_refused_not_clobbering() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(3), "Melee.zip")]).await;
    Mock::given(method("GET"))
        .and(path("/api/saves/3/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"raw bytes, not a zip".to_vec()))
        .mount(&server)
        .await;
    mock_complete(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = diff_env("dolphin", &t.path().join("me"));
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Melee", "ngc"), &env, None)
        .await
        .unwrap();
    assert_eq!(rep.failed.len(), 1);
    assert!(rep.failed[0].contains("another client"), "{:?}", rep.failed);
    assert!(!t.path().join("me").exists());
}

// ------------------------------------------------------------------ whole play sessions

#[cfg(unix)]
#[tokio::test]
async fn play_session_attributes_memory_card_changes_and_uploads_with_playtime() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 1, "operations": []})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mock_negotiate(&server, vec![op("upload", None, "Melee.zip")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(save_json(21, "Melee.zip")))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("dolphin-data");
    // A card for ANOTHER game already exists and must not be attributed to this one.
    std::fs::create_dir_all(root.join("GC/EUR")).unwrap();
    std::fs::write(root.join("GC/EUR/Card A.raw"), b"someone else's card").unwrap();
    let env = diff_env("dolphin", &root);

    // Stand-in emulator: writes a memory card and exits.
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(r#"mkdir -p "$0/GC/USA" && printf memcard-after-play > "$0/GC/USA/Card A.raw""#)
        .arg(root.to_str().unwrap());

    let c = client(&server);
    let mut st = State::default();
    let state_path = t.path().join("state.json");
    let out = romm_core::session::play(&c, DEVICE, &mut st, &state_path, t.path().join("bak"), &rom("Melee", "ngc"), &env, cmd, ConflictPolicy::KeepBoth, None)
        .await
        .unwrap();

    assert_eq!(out.exit_code, Some(0));
    assert_eq!(out.changed_files, ["GC/USA/Card A.raw"]);
    assert!(out.pull_error.is_none() && out.push_error.is_none(), "{out:?}");
    assert_eq!(out.push.as_ref().unwrap().uploaded, ["Melee.zip"]);

    // The uploaded zip holds only this game's card.
    let up = &reqs(&server, "POST", "/api/saves").await[0];
    assert!(up.body.windows(4).any(|w| w == b"PK\x03\x04"), "upload should be a zip");
    let body = String::from_utf8_lossy(&up.body);
    assert!(body.contains("GC/USA/Card A.raw") && !body.contains("GC/EUR"), "wrong files bundled");

    // Playtime rode along on the second session's completion.
    let done: Value = reqs(&server, "POST", "/api/sync/sessions/42/complete").await[0].body_json().unwrap();
    assert_eq!(done["play_sessions"][0]["rom_id"], 7);
    assert_eq!(done["play_sessions"][0]["save_slot"], "autosave");
    assert!(done["play_sessions"][0]["duration_ms"].is_u64());

    // State persisted for the next run.
    let reloaded = State::load(&state_path).unwrap();
    assert_eq!(reloaded.saves[&save_key(7, "dolphin")].files, ["GC/USA/Card A.raw"]);
    assert_eq!(reloaded.saves[&save_key(7, "dolphin")].server_save_id, Some(21));
}

#[cfg(unix)]
#[tokio::test]
async fn offline_play_still_runs_and_keeps_changes_for_next_sync() {
    // Nothing listens on port 9: every request fails immediately.
    let c = RommClient::new("http://127.0.0.1:9", Some("rmm_test".into()), false).unwrap();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("ppsspp-data");
    let env = diff_env("ppsspp", &root);
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(r#"mkdir -p "$0/SAVEDATA/ULUS10336DATA00" && printf s > "$0/SAVEDATA/ULUS10336DATA00/DATA.BIN""#)
        .arg(root.to_str().unwrap());

    let mut st = State::default();
    let state_path = t.path().join("state.json");
    let out = romm_core::session::play(&c, DEVICE, &mut st, &state_path, t.path().join("bak"), &rom("Daxter", "psp"), &env, cmd, ConflictPolicy::KeepBoth, None)
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0));
    assert!(out.pull_error.is_some() && out.push_error.is_some());
    // Tracked on disk so a later `sync` uploads it.
    let reloaded = State::load(&state_path).unwrap();
    assert_eq!(reloaded.saves[&save_key(7, "ppsspp")].files, ["SAVEDATA/ULUS10336DATA00/DATA.BIN"]);
}

#[tokio::test]
async fn emulator_that_fails_to_start_is_an_error() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = diff_env("pcsx2", &t.path().join("pcsx2"));
    let cmd = tokio::process::Command::new(t.path().join("does-not-exist"));
    let c = client(&server);
    let mut st = State::default();
    let err = romm_core::session::play(&c, DEVICE, &mut st, &t.path().join("s.json"), t.path().join("bak"), &rom("Kingdom Hearts", "ps2"), &env, cmd, ConflictPolicy::KeepBoth, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("could not start PCSX2"), "{err}");
}
