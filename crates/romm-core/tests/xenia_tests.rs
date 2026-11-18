//! Xbox 360 on Xenia, end to end against a mock RomM server: how a game is identified, and how its
//! saved-game folder is negotiated, uploaded, restored and remembered.
//!
//! The folder layout and the archive are Argosy's (`Xbox360FolderHandler`); `xenia.rs` has the unit
//! tests of the bytes, these check the protocol around them.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use romm_core::api::RommClient;
use romm_core::profiles;
use romm_core::route;
use romm_core::saves;
use romm_core::state::{save_key, LocalRom, SaveRecord, State};
use romm_core::sync::{saves_overview, ConflictPolicy, EmuEnv, SaveStatus, Syncer};
use romm_core::unit;
use romm_core::xenia;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const DEVICE: &str = "dev-1";
const SERVER_TIME: &str = "2026-01-02T03:04:05Z";
const TITLE_ID: u32 = 0x4D5307E6;
const TITLE: &str = "4D5307E6";
const XUID: &str = "E0001234567890AB";

// ------------------------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------------------------

fn env(root: &Path) -> EmuEnv {
    EmuEnv { profile: profiles::get("xenia").unwrap(), data_root: root.to_path_buf(), retroarch: None, core: None }
}

/// An extracted disc: `<dir>/<folder>/default.xex`, installed as a multi-file ROM is.
fn folder_game(dir: &Path, folder: &str, title: u32) -> LocalRom {
    let root = dir.join(folder);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("default.xex"), xex(title)).unwrap();
    LocalRom {
        rom_id: 7,
        name: folder.into(),
        platform_slug: "xbox360".into(),
        path: root.clone(),
        launch_path: root.join("default.xex"),
        stem: "default".into(), // what install_rom derives from the launch file
        ..Default::default()
    }
}

/// A single-file disc image: `<dir>/<file>`.
fn disc_game(dir: &Path, file: &str, title: u32) -> LocalRom {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(file);
    std::fs::write(&p, xbox360_disc(&xex(title))).unwrap();
    LocalRom {
        rom_id: 7,
        name: file.into(),
        platform_slug: "xbox360".into(),
        path: p.clone(),
        stem: p.file_stem().unwrap().to_string_lossy().into_owned(),
        launch_path: p,
        ..Default::default()
    }
}

fn put(root: &Path, rel: &str, data: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

/// `<storage>/content/<XUID>/<TITLE>/00000001/<rel>`
fn save_file(storage: &Path, rel: &str, data: &[u8]) {
    put(storage, &format!("content/{XUID}/{TITLE}/00000001/{rel}"), data);
}

fn saved_dir(storage: &Path) -> PathBuf {
    storage.join(format!("content/{XUID}/{TITLE}/00000001"))
}

/// The archive Argosy or another desktop would have uploaded for these files.
fn archive_of(files: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let t = tempfile::tempdir().unwrap();
    for (rel, data) in files {
        put(t.path(), &format!("00000001/{rel}"), data);
    }
    let c = xenia::collect(&t.path().join("00000001")).unwrap().unwrap();
    (c.data, c.content_hash)
}

fn stored_json(id: i64, name: &str, hash: &str) -> Value {
    json!({"id": id, "rom_id": 7, "file_name": name, "file_size_bytes": 1, "emulator": "xenia", "slot": "autosave", "content_hash": hash, "updated_at": SERVER_TIME})
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

async fn mock_negotiate_once(server: &MockServer, ops: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 42, "operations": ops})))
        .up_to_n_times(1)
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

async fn mock_download(server: &MockServer, id: i64, bytes: Vec<u8>) {
    Mock::given(method("GET")).and(path(format!("/api/saves/{id}/content"))).respond_with(ResponseTemplate::new(200).set_body_bytes(bytes)).mount(server).await;
}

async fn reqs(server: &MockServer, m: &str, p: &str) -> Vec<Request> {
    server.received_requests().await.unwrap().into_iter().filter(|r| r.method.as_str() == m && r.url.path() == p).collect()
}

fn client(server: &MockServer) -> RommClient {
    RommClient::new(&server.uri(), Some("rmm_test".into()), false).unwrap()
}

fn syncer<'a>(c: &'a RommClient, st: &'a mut State, bak: &Path, policy: ConflictPolicy) -> Syncer<'a> {
    Syncer { client: c, device_id: DEVICE, state: st, backup_dir: bak.to_path_buf(), policy }
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

// ------------------------------------------------------------------------------------------
// Knowing which game it is
// ------------------------------------------------------------------------------------------

#[test]
fn the_title_id_is_read_from_a_bare_xex_an_extracted_disc_and_a_disc_image() {
    let t = tempfile::tempdir().unwrap();
    let folder = folder_game(t.path(), "Halo 3", TITLE_ID);
    let disc = disc_game(t.path(), "Halo 3.iso", TITLE_ID);
    for (what, launch) in [("extracted disc (default.xex)", &folder.launch_path), ("disc image (.iso)", &disc.launch_path)] {
        let id = unit::identify(launch, "xbox360");
        assert_eq!(xenia::title_id_of(&id).as_deref(), Some(TITLE), "{what}: {id:?}");
    }
    // The id is the folder name Xenia writes under: eight upper-case hex digits.
    let id = unit::identify(&folder.launch_path, "xbox360");
    assert_eq!(id.title_id, TITLE);
}

#[test]
fn a_file_that_is_not_a_game_has_no_title_id() {
    let t = tempfile::tempdir().unwrap();
    // What a Games on Demand / STFS package starts with: not an XEX, not a disc image.
    put(t.path(), "pkg/LIVE-package", b"LIVE\0\0\0\0 not a disc");
    let id = unit::identify(&t.path().join("pkg/LIVE-package"), "xbox360");
    assert!(xenia::title_id_of(&id).is_none(), "{id:?}");
}

// ------------------------------------------------------------------------------------------
// Uploading
// ------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_local_save_is_negotiated_and_uploaded_as_argosys_archive() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"PROGRESS");
    save_file(&storage, "SAVE1/__thumbnail.png", b"PNG");
    let (_, expected) = archive_of(&[("SAVE1/data.bin", b"PROGRESS"), ("SAVE1/__thumbnail.png", b"PNG")]);

    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Halo 3.zip")]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(11, "Halo 3.zip", &expected))).mount(&server).await;
    mock_complete(&server).await;

    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();

    // Named after the game's folder, not `default.zip` after its launch file.
    assert_eq!(rep.uploaded, ["Halo 3.zip"], "{rep:?}");
    assert!(rep.is_clean());

    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    let s = &neg["saves"][0];
    assert_eq!(s["file_name"], "Halo 3.zip");
    assert_eq!(s["emulator"], "xenia");
    assert_eq!(s["slot"], "autosave");
    assert_eq!(s["content_hash"], expected.as_str(), "the hash must be the one Argosy's archive of the same folder has");

    let up = &reqs(&server, "POST", "/api/saves").await[0];
    let q: std::collections::HashMap<_, _> = up.url.query_pairs().into_owned().collect();
    assert_eq!((q["emulator"].as_str(), q["slot"].as_str(), q["content_hash"].as_str()), ("xenia", "autosave", expected.as_str()));
    let body = String::from_utf8_lossy(&up.body).into_owned();
    assert!(body.contains("00000001/SAVE1/data.bin"), "the archive is rooted at 00000001/");

    let rec = &st.saves[&save_key(7, "xenia")];
    assert_eq!((rec.last_hash.as_deref(), rec.server_save_id), (Some(expected.as_str()), Some(11)));
}

#[tokio::test]
async fn a_game_that_was_never_played_has_nothing_to_upload() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    // A profile and another game's save exist; this game has none.
    put(&storage, &format!("content/{XUID}/41560817/00000001/S/f"), b"x");
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert!(rep.is_clean() && rep.uploaded.is_empty(), "{rep:?}");
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"], json!([]));
}

#[tokio::test]
async fn a_single_file_disc_image_is_named_after_the_image() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "S/f", b"x");
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let game = disc_game(&t.path().join("roms"), "Halo 3.iso", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["file_name"], "Halo 3.zip");
}

#[tokio::test]
async fn a_moved_content_root_is_honoured() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(storage.join("xenia-canary.config.toml"), "[Storage]\ncontent_root = \"my-saves\"\n").unwrap();
    put(&storage, &format!("my-saves/{XUID}/{TITLE}/00000001/S/f"), b"MOVED");
    let (_, expected) = archive_of(&[("S/f", b"MOVED")]);

    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["content_hash"], expected.as_str());
}

// ------------------------------------------------------------------------------------------
// Downloading
// ------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_machine_gets_the_save_with_its_header_and_the_next_sync_is_quiet() {
    let (zip, server_hash) = archive_of(&[("SAVE1/data.bin", b"FROM-THE-OTHER-PC"), ("SAVE1/__thumbnail.png", b"PNG")]);
    let server = MockServer::start().await;
    mock_negotiate_once(&server, vec![op("download", Some(99), "Halo 3.zip")]).await;
    mock_negotiate(&server, vec![op("no_op", Some(99), "Halo 3.zip")]).await;
    mock_download(&server, 99, zip).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    // Xenia has been started once, so it made a profile; nothing else.
    std::fs::create_dir_all(storage.join(format!("content/{XUID}"))).unwrap();
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.downloaded, ["Halo 3.zip"], "{rep:?}");
    assert!(rep.is_clean());

    assert_eq!(std::fs::read(saved_dir(&storage).join("SAVE1/data.bin")).unwrap(), b"FROM-THE-OTHER-PC");
    // Xenia would not list the package without this.
    let header = storage.join(format!("content/{XUID}/{TITLE}/Headers/00000001/SAVE1.header"));
    assert_eq!(std::fs::read(&header).unwrap(), xenia::legacy_header(TITLE_ID, "SAVE1").unwrap());
    assert_eq!(st.saves[&save_key(7, "xenia")].last_hash.as_deref(), Some(server_hash.as_str()));
    // What was exchanged, taken now: the sync below counts as an exchange too, and when the server gives no
    // time it records the moment it happened, so reading this afterwards would compare against a later second.
    let recorded = st.saves[&save_key(7, "xenia")].server_updated_at.clone().unwrap();

    // Syncing again must not see the restored files as a change (that would upload them straight back).
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.unchanged, 1, "{rep:?}");
    let second: Value = reqs(&server, "POST", "/api/sync/negotiate").await[1].body_json().unwrap();
    assert_eq!(second["saves"][0]["content_hash"], server_hash.as_str());
    // While unchanged, what is reported back is what was last exchanged, so it can't look newer.
    assert_eq!(second["saves"][0]["updated_at"], recorded.as_str());
    assert!(reqs(&server, "POST", "/api/saves").await.is_empty(), "no upload");
}

#[tokio::test]
async fn restoring_keeps_a_copy_of_what_was_there_and_leaves_dlc_alone() {
    let (zip, _) = archive_of(&[("SAVE1/data.bin", b"SERVER")]);
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Halo 3.zip")]).await;
    mock_download(&server, 99, zip).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"LOCAL-PROGRESS");
    put(&storage, &format!("content/{XUID}/{TITLE}/00000002/DLC/pack.bin"), b"dlc");
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    // The local save was never synced, so the server's is a conflict; the policy decides.
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::ServerWins).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.downloaded, ["Halo 3.zip"], "{rep:?}");
    assert_eq!(std::fs::read(saved_dir(&storage).join("SAVE1/data.bin")).unwrap(), b"SERVER");
    assert_eq!(std::fs::read(storage.join(format!("content/{XUID}/{TITLE}/00000002/DLC/pack.bin"))).unwrap(), b"dlc");

    // The old save is on disk as the zip that would have been uploaded.
    let backups: Vec<_> = walk(&bak).into_iter().filter(|p| p.file_name().unwrap() == "Halo 3.zip").collect();
    assert_eq!(backups.len(), 1, "{:?}", walk(&bak));
    let kept = std::fs::read(&backups[0]).unwrap();
    assert!(saves::is_zip(&kept));
    let (expected, _) = archive_of(&[("SAVE1/data.bin", b"LOCAL-PROGRESS")]);
    assert_eq!(unit::content_hash(&kept), unit::content_hash(&expected));
}

#[tokio::test]
async fn with_no_profile_the_download_fails_plainly_and_is_not_recorded() {
    let (zip, _) = archive_of(&[("SAVE1/data.bin", b"SERVER")]);
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Halo 3.zip")]).await;
    mock_download(&server, 99, zip).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia"); // Xenia never run: no content folder at all
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.failed[0].contains("Halo 3") && rep.failed[0].contains("no profile"), "{:?}", rep.failed);
    assert!(rep.downloaded.is_empty());
    assert!(!st.saves.contains_key(&save_key(7, "xenia")), "a failed restore must not look synced");
    assert!(!storage.join("content").exists(), "nothing invented");
}

#[tokio::test]
async fn a_save_that_is_not_a_xenia_saved_game_is_refused_and_nothing_changes() {
    // Another emulator's (or an older romm-desktop's) unit under the same RomM game.
    let other = {
        let t = tempfile::tempdir().unwrap();
        put(t.path(), "SAVEDATA/ULUS10064/DATA.BIN", b"psp");
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        use std::io::Write;
        w.start_file("SAVEDATA/ULUS10064/DATA.BIN", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"psp").unwrap();
        w.finish().unwrap().into_inner()
    };
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Halo 3.zip")]).await;
    mock_download(&server, 99, other).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"MINE");
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::ServerWins).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.failed[0].contains("isn't a Xenia saved game"), "{:?}", rep.failed);
    assert_eq!(std::fs::read(saved_dir(&storage).join("SAVE1/data.bin")).unwrap(), b"MINE");
}

// ------------------------------------------------------------------------------------------
// Conflicts, deletion, the unidentifiable
// ------------------------------------------------------------------------------------------

#[tokio::test]
async fn keep_both_archives_the_local_folder_then_adopts_the_servers() {
    let (zip, _) = archive_of(&[("SAVE1/data.bin", b"SERVER")]);
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Halo 3.zip")]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(12, "archived", "h"))).mount(&server).await;
    mock_download(&server, 5, zip).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"LOCAL-PROGRESS");
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();

    let up = &reqs(&server, "POST", "/api/saves").await[0];
    let q: std::collections::HashMap<_, _> = up.url.query_pairs().into_owned().collect();
    assert!(!q.contains_key("slot"), "an archival save must not claim the slot");
    let body = String::from_utf8_lossy(&up.body).into_owned();
    // Named after the game, not after default.xex.
    assert!(body.contains("Halo 3.conflict-") && body.contains(".zip"), "{body}");
    assert!(!body.contains("default.conflict-"), "{body}");
    assert_eq!(std::fs::read(saved_dir(&storage).join("SAVE1/data.bin")).unwrap(), b"SERVER");
    assert_eq!(rep.archived.len(), 1);
    assert!(rep.is_clean(), "{rep:?}");
}

#[tokio::test]
async fn a_slot_emptied_on_the_server_untracks_the_save_but_never_deletes_the_folder() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("delete", Some(5), "Halo 3.zip")]).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"MINE");
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    st.saves.insert(save_key(7, "xenia"), SaveRecord { last_hash: Some("old".into()), server_save_id: Some(5), ..Default::default() });
    syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(std::fs::read(saved_dir(&storage).join("SAVE1/data.bin")).unwrap(), b"MINE", "Xenia's folder is never deleted");
    assert!(!st.saves.contains_key(&save_key(7, "xenia")), "but it is no longer tracked");
}

#[tokio::test]
async fn a_game_whose_id_cannot_be_read_says_so_and_syncs_nothing() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"MINE");
    // A package Sigil can't open.
    put(t.path(), "roms/Halo 3 GoD/LIVE", b"LIVE\0\0\0\0");
    let game = LocalRom {
        rom_id: 7,
        name: "Halo 3 GoD".into(),
        platform_slug: "xbox360".into(),
        path: t.path().join("roms/Halo 3 GoD"),
        launch_path: t.path().join("roms/Halo 3 GoD/LIVE"),
        stem: "LIVE".into(),
        ..Default::default()
    };
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    let msg = &rep.failed[0];
    assert!(msg.contains("Halo 3 GoD") && msg.contains("title id") && msg.contains("Games on Demand"), "{msg}");
    assert!(rep.uploaded.is_empty() && reqs(&server, "POST", "/api/saves").await.is_empty());
}

// ------------------------------------------------------------------------------------------
// Local view and routing
// ------------------------------------------------------------------------------------------

#[test]
fn the_saves_overview_follows_never_synced_synced_modified() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"A");
    save_file(&storage, "SAVE1/__thumbnail.png", b"B");
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let e = env(&storage);
    let mut st = State::default();

    let v = saves_overview(&e, &game, &st).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!((v[0].name.as_str(), v[0].status, v[0].detail.as_deref()), ("Halo 3.zip", SaveStatus::NeverSynced, Some("2 files")));

    let (_, hash) = archive_of(&[("SAVE1/data.bin", b"A"), ("SAVE1/__thumbnail.png", b"B")]);
    st.saves.insert(save_key(7, "xenia"), SaveRecord { last_hash: Some(hash), ..Default::default() });
    assert_eq!(saves_overview(&e, &game, &st).unwrap()[0].status, SaveStatus::Synced);

    save_file(&storage, "SAVE1/data.bin", b"A+");
    assert_eq!(saves_overview(&e, &game, &st).unwrap()[0].status, SaveStatus::Modified);
}

#[test]
fn a_game_with_no_saves_has_an_empty_overview() {
    let t = tempfile::tempdir().unwrap();
    let game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    assert!(saves_overview(&env(&t.path().join("xenia")), &game, &State::default()).unwrap().is_empty());
}

#[test]
fn xenia_only_plans_saves_for_xbox_360_games() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let mut game = folder_game(&t.path().join("roms"), "Halo 3", TITLE_ID);
    let id = unit::identify(&game.launch_path, "xbox360");
    let plan = route::plan(&e, &game, &id).expect("an Xbox 360 game under Xenia has a save plan");
    assert_eq!(plan.label, "xenia");
    assert!(matches!(&plan.kind, route::Kind::Folder { title_id, .. } if title_id == TITLE));
    // An original Xbox game belongs to xemu, which syncs nothing.
    game.platform_slug = "xbox".into();
    assert!(route::plan(&e, &game, &id).is_none());
}

// ------------------------------------------------------------------------------------------
// Multi-disc games (a playlist is the launch file)
// ------------------------------------------------------------------------------------------

/// `<dir>/MyGame/{MyGame (Disk 1).iso, MyGame (Disk 2).iso, MyGame.m3u}`, as RomM serves it and
/// install_rom stores it: the playlist is the launch file, the folder is the install.
fn multi_disc_game(dir: &Path, title: u32) -> LocalRom {
    let root = dir.join("MyGame");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("MyGame (Disk 1).iso"), xbox360_disc(&xex(title))).unwrap();
    std::fs::write(root.join("MyGame (Disk 2).iso"), xbox360_disc(&xex(title))).unwrap();
    std::fs::write(root.join("MyGame.m3u"), "MyGame (Disk 1).iso\nMyGame (Disk 2).iso\n").unwrap();
    LocalRom {
        rom_id: 7,
        name: "MyGame".into(),
        platform_slug: "xbox360".into(),
        path: root.clone(),
        launch_path: root.join("MyGame.m3u"),
        stem: "MyGame".into(),
        ..Default::default()
    }
}

#[test]
fn a_playlist_identifies_the_game_from_its_first_disc() {
    let t = tempfile::tempdir().unwrap();
    let game = multi_disc_game(t.path(), TITLE_ID);
    let id = unit::identify(&game.launch_path, "xbox360");
    assert_eq!(xenia::title_id_of(&id).as_deref(), Some(TITLE), "{id:?}");
}

/// The save side never needed the playlist resolved at install: Sigil follows it, so a multi-disc game
/// syncs like any other and is named after its folder.
#[tokio::test]
async fn a_multi_disc_game_syncs_its_saves_through_the_playlist() {
    let t = tempfile::tempdir().unwrap();
    let storage = t.path().join("xenia");
    save_file(&storage, "SAVE1/data.bin", b"DISC-2-PROGRESS");
    let (_, expected) = archive_of(&[("SAVE1/data.bin", b"DISC-2-PROGRESS")]);
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "MyGame.zip")]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(11, "MyGame.zip", &expected))).mount(&server).await;
    mock_complete(&server).await;

    let game = multi_disc_game(&t.path().join("roms"), TITLE_ID);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&game, &env(&storage), None).await.unwrap();
    assert_eq!(rep.uploaded, ["MyGame.zip"], "{rep:?}");
    assert!(rep.is_clean(), "{rep:?}");
}
