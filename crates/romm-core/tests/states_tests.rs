//! Save-state sync against a mock RomM server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use romm_core::api::RommClient;
use romm_core::paths::{Os, PathCtx};
use romm_core::profiles;
use romm_core::retroarch;
use romm_core::saves::md5_bytes;
use romm_core::state::{state_key, LocalRom, State, StateRecord};
use romm_core::states::{overview, sync_states, StateOptions, StateStatus};
use romm_core::sync::{ConflictPolicy, EmuEnv};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn ra_env(root: &Path) -> EmuEnv {
    EmuEnv {
        profile: profiles::get("retroarch").unwrap(),
        data_root: root.to_path_buf(),
        retroarch: Some(retroarch::layout(root, Os::Linux, &PathCtx::default())),
    }
}

fn diff_env(id: &str, root: &Path) -> EmuEnv {
    EmuEnv { profile: profiles::get(id).unwrap(), data_root: root.to_path_buf(), retroarch: None }
}

fn rom(stem: &str) -> LocalRom {
    LocalRom { rom_id: 7, name: stem.into(), platform_slug: "snes".into(), path: PathBuf::from("/r"), launch_path: PathBuf::from("/r/x"), stem: stem.into(), ..Default::default() }
}

fn st(id: i64, name: &str, size: u64, updated: &str, emu: &str) -> Value {
    json!({"id": id, "rom_id": 7, "file_name": name, "file_size_bytes": size, "emulator": emu, "updated_at": updated, "missing_from_fs": false, "core": null})
}

async fn mock_list(server: &MockServer, items: Vec<Value>) {
    Mock::given(method("GET")).and(path("/api/states")).and(query_param("rom_id", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(items)).mount(server).await;
}

async fn mock_content(server: &MockServer, id: i64, bytes: &[u8]) {
    Mock::given(method("GET")).and(path(format!("/api/states/{id}/content")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec())).mount(server).await;
}

async fn reqs(server: &MockServer, m: &str, re: &str) -> Vec<Request> {
    let re = regex_lite(re);
    server.received_requests().await.unwrap().into_iter()
        .filter(|r| r.method.as_str() == m && re(r.url.path())).collect()
}
// tiny matcher: exact path, or prefix when the pattern ends with '*'
fn regex_lite(p: &str) -> impl Fn(&str) -> bool + '_ {
    move |actual| match p.strip_suffix('*') { Some(pre) => actual.starts_with(pre), None => actual == p }
}

fn client(s: &MockServer) -> RommClient { RommClient::new(&s.uri(), Some("rmm_t".into()), false).unwrap() }
fn q(r: &Request) -> HashMap<String, String> { r.url.query_pairs().into_owned().collect() }
fn opts() -> StateOptions { StateOptions::default() }

fn write(p: &Path, b: &[u8]) { std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(p, b).unwrap(); }

fn baseline(db: &mut State, name: &str, local: &[u8], updated: &str, size: u64, id: i64) {
    db.states.insert(state_key(7, "retroarch", name), StateRecord {
        server_id: Some(id), local_hash: md5_bytes(local), server_updated_at: updated.into(), server_size: size, ..Default::default()
    });
}

fn contains(dir: &Path, needle: &[u8]) -> bool {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) { for e in rd.flatten() { let p = e.path(); if p.is_dir() { walk(&p, out) } else { out.push(p) } } }
    }
    let mut v = vec![]; walk(dir, &mut v);
    v.iter().any(|p| std::fs::read(p).map(|b| b == needle).unwrap_or(false))
}

#[tokio::test]
async fn new_local_state_is_uploaded_with_its_thumbnail_and_baselined() {
    let server = MockServer::start().await;
    mock_list(&server, vec![]).await;
    Mock::given(method("POST")).and(path("/api/states"))
        .respond_with(ResponseTemplate::new(201).set_body_json(st(5, "Zelda.state1", 2, "T1", "retroarch"))).mount(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"v1");
    write(&t.path().join("ra/states/Zelda.state1.png"), b"PNGDATA");
    let mut db = State::default();
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();

    assert_eq!(rep.uploaded, ["Zelda.state1"], "the .png is a thumbnail, not a state");
    let up = &reqs(&server, "POST", "/api/states").await[0];
    assert_eq!(q(up)["rom_id"], "7");
    assert_eq!(q(up)["emulator"], "retroarch");
    let body = String::from_utf8_lossy(&up.body);
    assert!(body.contains("name=\"stateFile\"") && body.contains("filename=\"Zelda.state1\"") && body.contains("v1"), "{body}");
    assert!(body.contains("name=\"screenshotFile\"") && body.contains("PNGDATA"), "thumbnail must ride along");
    let rec = &db.states[&state_key(7, "retroarch", "Zelda.state1")];
    assert_eq!((rec.server_id, rec.server_updated_at.as_str(), rec.local_hash.as_str()), (Some(5), "T1", md5_bytes(b"v1").as_str()));
}

#[tokio::test]
async fn fresh_device_downloads_only_this_emulators_states() {
    let server = MockServer::start().await;
    mock_list(&server, vec![
        st(1, "Zelda.state1", 3, "T1", "retroarch"),
        st(2, "Zelda.state.auto", 4, "T1", "retroarch"),
        st(3, "Zelda.state9", 5, "T1", "emulatorjs"),                          // another emulator's
        st(4, "Zelda.state2.conflict-20260101T000000", 6, "T1", "retroarch"), // archived conflict copy
    ]).await;
    mock_content(&server, 1, b"one").await;
    mock_content(&server, 2, b"auto").await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    let mut db = State::default();
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();

    assert_eq!(rep.downloaded, ["Zelda.state.auto", "Zelda.state1"]);
    assert_eq!(std::fs::read(t.path().join("ra/states/Zelda.state1")).unwrap(), b"one");
    assert_eq!(std::fs::read(t.path().join("ra/states/Zelda.state.auto")).unwrap(), b"auto");
    assert!(!t.path().join("ra/states/Zelda.state9").exists() && !t.path().join("ra/states/Zelda.state2.conflict-20260101T000000").exists());
    assert!(reqs(&server, "GET", "/api/states/3/content").await.is_empty());
    assert!(!t.path().join("ra/states/Zelda.state1.romm-dl").exists(), "temp file must be gone");
    assert_eq!(db.states[&state_key(7, "retroarch", "Zelda.state1")].server_updated_at, "T1");
}

#[tokio::test]
async fn only_local_changed_replaces_the_server_copy_in_place() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 2, "T1", "retroarch")]).await;
    Mock::given(method("PUT")).and(path("/api/states/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(st(5, "Zelda.state1", 2, "T2", "retroarch"))).mount(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"v2");
    let mut db = State::default();
    baseline(&mut db, "Zelda.state1", b"v1", "T1", 2, 5);
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();
    assert_eq!(rep.uploaded, ["Zelda.state1"]);
    assert!(reqs(&server, "POST", "/api/states").await.is_empty(), "must update, not create a duplicate");
    let rec = &db.states[&state_key(7, "retroarch", "Zelda.state1")];
    assert_eq!((rec.server_updated_at.as_str(), rec.local_hash.as_str()), ("T2", md5_bytes(b"v2").as_str()));
}

#[tokio::test]
async fn only_server_changed_downloads_and_backs_up_the_old_local_file() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 6, "T2", "retroarch")]).await;
    mock_content(&server, 5, b"newer!").await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"v1");
    let mut db = State::default();
    baseline(&mut db, "Zelda.state1", b"v1", "T1", 2, 5);
    let bak = t.path().join("bak");
    let rep = sync_states(&client(&server), &mut db, &bak, &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();
    assert_eq!(rep.downloaded, ["Zelda.state1"]);
    assert_eq!(std::fs::read(t.path().join("ra/states/Zelda.state1")).unwrap(), b"newer!");
    assert!(contains(&bak, b"v1"), "previous local state must be backed up");
}

#[tokio::test]
async fn identical_files_seen_for_the_first_time_are_verified_not_rewritten() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 2, "T1", "retroarch")]).await;
    mock_content(&server, 5, b"v1").await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"v1");
    let mut db = State::default();
    let bak = t.path().join("bak");
    let rep = sync_states(&client(&server), &mut db, &bak, &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();
    assert_eq!((rep.unchanged, rep.uploaded.len(), rep.downloaded.len()), (1, 0, 0));
    assert!(reqs(&server, "PUT", "/api/states/*").await.is_empty() && reqs(&server, "POST", "/api/states").await.is_empty());
    assert!(!t.path().join("ra/states/Zelda.state1.romm-dl").exists());
    assert!(!bak.exists(), "nothing was overwritten, so nothing to back up");
    assert!(db.states.contains_key(&state_key(7, "retroarch", "Zelda.state1")), "baseline recorded for next time");
}

#[tokio::test]
async fn different_files_seen_for_the_first_time_keep_both() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 6, "T1", "retroarch")]).await;
    mock_content(&server, 5, b"SERVER").await;
    Mock::given(method("POST")).and(path("/api/states"))
        .respond_with(ResponseTemplate::new(201).set_body_json(st(9, "archived", 5, "T1", "retroarch"))).mount(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"LOCAL");
    let mut db = State::default();
    let bak = t.path().join("bak");
    let rep = sync_states(&client(&server), &mut db, &bak, &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();

    let up = &reqs(&server, "POST", "/api/states").await[0];
    let body = String::from_utf8_lossy(&up.body);
    assert!(body.contains("filename=\"Zelda.state1.conflict-") && body.contains("LOCAL"), "local copy archived under a conflict name: {body}");
    assert_eq!(std::fs::read(t.path().join("ra/states/Zelda.state1")).unwrap(), b"SERVER");
    assert!(contains(&bak, b"LOCAL"));
    assert_eq!((rep.archived.len(), rep.downloaded.len(), rep.failed.len()), (1, 1, 0));
}

#[tokio::test]
async fn both_changed_with_ask_policy_touches_nothing() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 6, "T2", "retroarch")]).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"LOCAL2");
    let mut db = State::default();
    baseline(&mut db, "Zelda.state1", b"v1", "T1", 2, 5);
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::Ask, &opts()).await.unwrap();
    assert_eq!(rep.conflicts, ["Zelda.state1"]);
    assert_eq!(std::fs::read(t.path().join("ra/states/Zelda.state1")).unwrap(), b"LOCAL2");
    assert_eq!(reqs(&server, "GET", "/api/states/5/content").await.len() + reqs(&server, "POST", "/api/states").await.len() + reqs(&server, "PUT", "/api/states/5").await.len(), 0);
}

#[tokio::test]
async fn both_changed_device_wins_overwrites_the_server() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 6, "T2", "retroarch")]).await;
    Mock::given(method("PUT")).and(path("/api/states/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(st(5, "Zelda.state1", 6, "T3", "retroarch"))).mount(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), b"LOCAL2");
    let mut db = State::default();
    baseline(&mut db, "Zelda.state1", b"v1", "T1", 2, 5);
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::DeviceWins, &opts()).await.unwrap();
    assert_eq!(rep.uploaded, ["Zelda.state1"]);
    assert_eq!(db.states[&state_key(7, "retroarch", "Zelda.state1")].server_updated_at, "T3");
}

#[tokio::test]
async fn a_state_deleted_locally_stays_deleted_until_the_server_copy_changes() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(5, "Zelda.state1", 2, "T1", "retroarch")]).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    let mut db = State::default();
    baseline(&mut db, "Zelda.state1", b"v1", "T1", 2, 5);
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();
    assert!(rep.downloaded.is_empty());
    assert!(reqs(&server, "GET", "/api/states/5/content").await.is_empty());
}

#[tokio::test]
async fn oversized_states_are_skipped_in_both_directions() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(8, "Zelda.state2", 5000, "T1", "retroarch")]).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    write(&t.path().join("ra/states/Zelda.state1"), &[0u8; 5000]);
    let mut db = State::default();
    let small = StateOptions { max_bytes: 1000 };
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &small).await.unwrap();
    assert_eq!(rep.skipped, ["Zelda.state1", "Zelda.state2"]);
    assert!(reqs(&server, "POST", "/api/states").await.is_empty() && reqs(&server, "GET", "/api/states/8/content").await.is_empty());
}

#[tokio::test]
async fn retroarch_state_sorting_blocks_new_downloads_with_a_clear_reason() {
    let server = MockServer::start().await;
    mock_list(&server, vec![st(1, "Zelda.state1", 3, "T1", "retroarch")]).await;
    mock_content(&server, 1, b"one").await;
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("ra");
    write(&root.join("retroarch.cfg"), b"sort_savestates_enable = \"true\"\n");
    let env = ra_env(&root);
    let mut db = State::default();
    let rep = sync_states(&client(&server), &mut db, &t.path().join("bak"), &rom("Zelda"), &env, ConflictPolicy::KeepBoth, &opts()).await.unwrap();
    assert_eq!(rep.failed.len(), 1);
    assert!(rep.failed[0].contains("sorts save states"), "{:?}", rep.failed);
    assert!(!root.join("states/Zelda.state1").exists());
}

#[tokio::test]
async fn overview_reports_each_status_without_changing_anything() {
    let server = MockServer::start().await;
    mock_list(&server, vec![
        st(1, "Zelda.state1", 2, "T1", "retroarch"),   // in sync
        st(2, "Zelda.state2", 2, "T9", "retroarch"),   // server newer
        st(3, "Zelda.state3", 2, "T1", "retroarch"),   // local newer
        st(4, "Zelda.state4", 2, "T1", "retroarch"),   // server only
        st(5, "Zelda.state5", 2, "T1", "retroarch"),   // unverified
    ]).await;
    let t = tempfile::tempdir().unwrap();
    let env = ra_env(&t.path().join("ra"));
    let mut db = State::default();
    for (n, local, base_hash) in [("Zelda.state1", &b"aa"[..], &b"aa"[..]), ("Zelda.state2", b"bb", b"bb"), ("Zelda.state3", b"NEW", b"old")] {
        write(&t.path().join("ra/states").join(n), local);
        baseline(&mut db, n, base_hash, "T1", 2, 1);
    }
    write(&t.path().join("ra/states/Zelda.state5"), b"cc");
    write(&t.path().join("ra/states/Zelda.state6"), b"only here");
    let views = overview(Some(&client(&server)), &db, &rom("Zelda"), &env).await.unwrap();
    let status: HashMap<_, _> = views.iter().map(|v| (v.name.as_str(), v.status)).collect();
    assert_eq!(status["Zelda.state1"], StateStatus::InSync);
    assert_eq!(status["Zelda.state2"], StateStatus::ServerNewer);
    assert_eq!(status["Zelda.state3"], StateStatus::LocalNewer);
    assert_eq!(status["Zelda.state4"], StateStatus::ServerOnly);
    assert_eq!(status["Zelda.state5"], StateStatus::Unverified);
    assert_eq!(status["Zelda.state6"], StateStatus::LocalOnly);
    assert!(reqs(&server, "GET", "/api/states/*").await.is_empty(), "overview must not download anything");
    // Offline: local files only.
    let off = overview(None, &db, &rom("Zelda"), &env).await.unwrap();
    assert!(off.iter().all(|v| matches!(v.status, StateStatus::LocalOnly | StateStatus::ServerOnly)));
}

#[cfg(unix)]
#[tokio::test]
async fn dolphin_session_attributes_a_new_state_file_and_uploads_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 1, "operations": []}))).mount(&server).await;
    Mock::given(method("POST")).and(path_regex(r"^/api/sync/sessions/\d+/complete$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&server).await;
    mock_list(&server, vec![]).await;
    Mock::given(method("POST")).and(path("/api/states"))
        .respond_with(ResponseTemplate::new(201).set_body_json(st(31, "GALE01.s01", 5, "T1", "dolphin"))).mount(&server).await;

    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("dolphin");
    // A state belonging to another game already exists and must not be attributed to this one.
    write(&root.join("StateSaves/GM4E01.s01"), b"other game");
    let env = diff_env("dolphin", &root);
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(r#"mkdir -p "$0/StateSaves"; printf slot1 > "$0/StateSaves/GALE01.s01""#)
        .arg(root.to_str().unwrap());

    let c = client(&server);
    let mut db = State::default();
    let r = rom("Melee");
    let out = romm_core::session::play(&c, "dev", &mut db, &t.path().join("s.json"), t.path().join("bak"), &r, &env, cmd, ConflictPolicy::KeepBoth, Some(&opts())).await.unwrap();

    assert_eq!(out.changed_states, ["GALE01.s01"], "another game's existing state must not be attributed to this one");
    assert_eq!(out.state_push.as_ref().unwrap().uploaded, ["GALE01.s01"]);
    assert!(out.state_error.is_none(), "{out:?}");
    let up = &reqs(&server, "POST", "/api/states").await[0];
    assert_eq!(q(up)["emulator"], "dolphin");
    assert!(String::from_utf8_lossy(&up.body).contains("slot1"));
    assert_eq!(db.state_names["7:dolphin"], ["GALE01.s01"]);
    // Persisted for next launch.
    let again = State::load(&t.path().join("s.json")).unwrap();
    assert!(again.states.contains_key(&state_key(7, "dolphin", "GALE01.s01")));
}

#[test]
fn screenshots_stored_next_to_ppsspp_states_are_ignored() {
    let t = tempfile::tempdir().unwrap();
    let env = diff_env("ppsspp", t.path());
    assert!(env.state_ignored("ULUS10336_1.01_0.jpg") && env.state_ignored("x.PNG"));
    assert!(!env.state_ignored("ULUS10336_1.01_0.ppst"));
    assert!(!diff_env("dolphin", t.path()).state_ignored("GALE01.s01"));
}
