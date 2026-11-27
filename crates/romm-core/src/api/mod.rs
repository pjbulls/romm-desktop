//! Typed client for the RomM REST API.

pub mod types;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::{multipart, Response, Url};
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use types::*;

pub const CLIENT_SLUG: &str = "romm-desktop";

/// The scopes a token needs for everything this app does. A token made in RomM (Profile → Client
/// API Tokens) without one of them is accepted at login and then fails when that feature is used.
pub const REQUIRED_SCOPES: &[&str] = &["me.read", "platforms.read", "roms.read", "firmware.read", "assets.read", "assets.write", "devices.read", "devices.write", "roms.user.write"];

/// The required scopes that `granted` does not include, in `REQUIRED_SCOPES` order.
pub fn missing_scopes(granted: &[String]) -> Vec<&'static str> {
    REQUIRED_SCOPES.iter().copied().filter(|needed| !granted.iter().any(|g| g == needed)).collect()
}

/// A pairing code as RomM wants it: letters and digits, upper case. RomM shows it as `1234-5678`
/// and people paste it with the dash or with spaces.
pub fn normalize_pair_code(input: &str) -> String {
    input.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_uppercase()
}

/// The whole time an upload of `bytes` may take.
fn upload_limit(bytes: u64) -> Duration {
    UPLOAD_BASE_LIMIT + Duration::from_secs(bytes / UPLOAD_MIN_RATE)
}

/// How long to wait before each retry of a call that is safe to repeat (see `send_retrying`).
const RETRY_WAITS_MS: [u64; 2] = [150, 450];

/// The longest a request waits for another program's turn before going ahead regardless.
const PROCESS_LOCK_WAIT: Duration = Duration::from_secs(10);

/// How long a connection may take to open. A server that is down or unreachable fails after this,
/// not after the operating system's own limit (minutes).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a response may go silent between two reads of its body. A download that is moving, however
/// slowly, is never cut off by this; one that has stopped is.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a server may take to begin answering (its response headers). `reqwest` has no timeout
/// for this step that leaves the body alone, and everything that holds a turn (see `send`) is
/// waiting on it.
const HEADER_WAIT: Duration = Duration::from_secs(60);
/// How long an upload keeps its turn. RomM's auth step runs when a request *starts* (it is
/// Starlette middleware, ahead of the body), so a request needs its turn only until that is past,
/// not until a big body has gone out and been answered.
const UPLOAD_TURN: Duration = Duration::from_secs(2);
/// An upload's whole time limit is this plus its size at `UPLOAD_MIN_RATE`: slow links finish,
/// a server that has stopped reading does not hold the sync that is waiting on it for ever.
const UPLOAD_BASE_LIMIT: Duration = Duration::from_secs(60);
const UPLOAD_MIN_RATE: u64 = 50_000; // bytes per second

/// Accepts `192.168.1.10:8080`, `http://host`, `https://host/` and so on.
///
/// A scheme that was typed is kept: someone who writes `http://` means it (a test server, a
/// reverse proxy that terminates TLS elsewhere). When there is none, the sign-in carries a token,
/// so the default is `https://`, except for hosts that are on the user's own machine or network
/// (see `is_local_host`), where a certificate is unlikely and a self-hosted RomM at
/// `192.168.1.10:8080` is the normal case.
pub fn normalize_base_url(input: &str) -> Result<String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err(Error::msg("server URL is empty"));
    }
    let with_scheme = if s.contains("://") {
        s.to_string()
    } else {
        let probe = Url::parse(&format!("http://{s}")).map_err(|e| Error::msg(format!("invalid server URL: {e}")))?;
        let scheme = if probe.host_str().is_some_and(is_local_host) { "http" } else { "https" };
        format!("{scheme}://{s}")
    };
    Url::parse(&with_scheme).map_err(|e| Error::msg(format!("invalid server URL: {e}")))?;
    Ok(with_scheme)
}

/// Whether `host` (as a URL writes it: a name, an IPv4 address or a bracketed IPv6 one) is on this
/// machine or the network it is on: loopback, private and link-local addresses, Tailscale's
/// 100.64.0.0/10, a name with no dot (`romm`, `nas`) or ending in `.local`, `.lan`, `.home`,
/// `.internal`, `.localdomain`, `.home.arpa` or `.localhost`. Nobody can get a certificate for
/// most of these, so `http://` is what a person means by them.
pub fn is_local_host(host: &str) -> bool {
    fn local_v4(v: std::net::Ipv4Addr) -> bool {
        let o = v.octets();
        v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || (o[0] == 100 && (64..128).contains(&o[1]))
    }
    let h = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
    if let Ok(ip) = h.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v) => local_v4(v),
            IpAddr::V6(v) => {
                let s = v.segments()[0];
                v.is_loopback() || v.is_unspecified() || (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80 || v.to_ipv4_mapped().is_some_and(local_v4)
            }
        };
    }
    !h.contains('.') || ["local", "lan", "home", "internal", "localdomain", "home.arpa", "localhost"].iter().any(|d| h.ends_with(&format!(".{d}")))
}

/// What to tell someone who signs in to a server over plain `http://` that is not on their own
/// network: the token, and every save and ROM after it, travel in the clear. A warning and not a
/// refusal, because they chose it.
pub fn cleartext_warning(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?;
    if url.scheme() != "http" || url.host_str().is_some_and(is_local_host) {
        return None;
    }
    Some(format!(
        "{} is reached over plain http, so anyone between you and it can read the token and everything you sync. Use an https:// address if the server has one.",
        url.host_str().unwrap_or("This server")
    ))
}

#[derive(Clone)]
pub struct RommClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    /// Shared by every clone: API calls take turns until their response headers arrive. See `send`.
    gate: Arc<Mutex<()>>,
    /// A file that every program using this app's token locks while it waits for headers, so the
    /// desktop app and the CLI (one token, two processes) take turns as well. See `send`.
    process_lock: Option<Arc<PathBuf>>,
    /// How long to wait for that lock before going ahead without it.
    lock_wait: Duration,
    /// How long a server may take to begin answering; see `HEADER_WAIT`.
    header_wait: Duration,
    /// How long an upload keeps its turn; see `UPLOAD_TURN`.
    upload_turn: Duration,
    /// What the server can do, and when that was read; shared by every clone. See `capabilities`.
    caps: Arc<StdMutex<Option<(Instant, Capabilities)>>>,
}

/// How long what the server can do is trusted before the heartbeat is asked again. A server is
/// upgraded rarely, and a sync asks once per game.
const CAPABILITIES_TTL: Duration = Duration::from_secs(300);

impl RommClient {
    pub fn new(base_url: &str, token: Option<String>, accept_invalid_certs: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(format!("{CLIENT_SLUG}/{}", env!("CARGO_PKG_VERSION")))
            .danger_accept_invalid_certs(accept_invalid_certs)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .tcp_keepalive(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            base: normalize_base_url(base_url)?,
            token,
            gate: Arc::new(Mutex::new(())),
            process_lock: None,
            lock_wait: PROCESS_LOCK_WAIT,
            header_wait: HEADER_WAIT,
            upload_turn: UPLOAD_TURN,
            caps: Arc::new(StdMutex::new(None)),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Makes this client take turns, with every other program that uses the same lock file (the
    /// desktop app and the CLI share a token), until each one's response headers arrive. Without it,
    /// two programs can reach RomM's auth together and one of them gets a 500; the in-process lock
    /// cannot help there. If the file can't be locked, requests simply go ahead.
    pub fn with_process_lock(mut self, path: impl Into<PathBuf>) -> Self {
        self.process_lock = Some(Arc::new(path.into()));
        self
    }

    #[doc(hidden)]
    pub fn with_process_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    #[doc(hidden)]
    pub fn with_header_wait(mut self, wait: Duration) -> Self {
        self.header_wait = wait;
        self
    }

    #[doc(hidden)]
    pub fn with_upload_turn(mut self, turn: Duration) -> Self {
        self.upload_turn = turn;
        self
    }

    /// `{base}/api/{segments...}` with every segment percent-encoded.
    fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(&self.base).map_err(|e| Error::msg(e.to_string()))?;
        {
            let mut p = url
                .path_segments_mut()
                .map_err(|_| Error::msg("server URL cannot be a base"))?;
            p.pop_if_empty().push("api");
            for s in segments {
                p.push(s);
            }
        }
        Ok(url)
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// Sends one `/api` request and returns once the response *headers* are in.
    ///
    /// For every request made with a client token, RomM's auth middleware updates the token's,
    /// the device's and the user's "last seen" rows (the token and device ones at most every
    /// 5 minutes). When two requests arrive together while those are due, they race to update
    /// the same rows, and on MariaDB with snapshot isolation the loser gets an HTTP 500 (error
    /// 1020, "Record has changed since last read in table 'client_tokens'"). The app issues
    /// several calls at once (library page + platforms, background sync, details panel), so
    /// they take turns here: the auth step of one finishes before the next one starts.
    /// Bodies and downloads stream after the lock is released, so only the wait for headers is
    /// serialised, and that wait is bounded (`HEADER_WAIT`): a server that never answers fails
    /// the call instead of holding every other call, in this program and in the CLI, behind it.
    /// Uploads, whose headers come only after the body has gone out, use `send_upload`.
    /// Not used for `/assets` (served by nginx, never reaches RomM's auth).
    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Response> {
        let _turn = self.gate.lock().await;
        let _other_programs = self.process_turn().await;
        self.headers(rb).await
    }

    /// `send` for a request that carries `bytes` of body. It keeps its turn only for `upload_turn`,
    /// which is how long RomM's auth step needs after a request starts, and then lets the next
    /// request go while the body is still being sent; holding it to the end made one large state
    /// upload stop every other call for as long as it took. The upload as a whole is limited by its
    /// size (`UPLOAD_BASE_LIMIT` plus `UPLOAD_MIN_RATE`).
    async fn send_upload(&self, rb: reqwest::RequestBuilder, bytes: u64) -> Result<Response> {
        let turn = self.gate.lock().await;
        let other_programs = self.process_turn().await;
        let limit = upload_limit(bytes);
        let sending = rb.send();
        tokio::pin!(sending);
        let done = tokio::time::timeout(limit, async {
            let early = tokio::select! {
                r = &mut sending => Some(r),
                () = tokio::time::sleep(self.upload_turn) => None,
            };
            drop((turn, other_programs));
            match early {
                Some(r) => r,
                None => sending.await,
            }
        })
        .await;
        match done {
            Ok(r) => Ok(r?),
            Err(_) => Err(Error::msg(format!("the upload did not finish within {} seconds", limit.as_secs()))),
        }
    }

    /// Waits for the response headers, but not for ever. This is the one place that does; every
    /// request goes through it.
    async fn headers(&self, rb: reqwest::RequestBuilder) -> Result<Response> {
        match tokio::time::timeout(self.header_wait, rb.send()).await {
            Ok(r) => Ok(r?),
            Err(_) => Err(Error::msg(format!("the server did not answer within {} seconds", self.header_wait.as_secs()))),
        }
    }

    /// The cross-program half of the turn-taking: an exclusive lock on the shared lock file, held
    /// (by keeping the returned file) until the caller is done waiting for headers. The operating
    /// system drops it if a program dies. It never makes a request wait for good: a lock that can't
    /// be had within `lock_wait`, or a file that can't be locked at all, means going ahead without.
    async fn process_turn(&self) -> Option<std::fs::File> {
        let path = self.process_lock.as_ref()?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path.as_path()).ok()?;
        let give_up = std::time::Instant::now() + self.lock_wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Some(file),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < give_up => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(_) => return None,
            }
        }
    }

    /// `send`, repeated (twice at most, after a short wait) when the server answers with an error
    /// that can be a passing glitch: the auth race above is one, and the app sends nothing else at
    /// the same moment from this process, so it can still come from a second program using the same
    /// token (the CLI, a second window) or from RomM's own background work.
    ///
    /// Only for calls that are safe to make twice: reads, registering a device, negotiating and
    /// completing a sync session. Never for uploads, which could be stored twice. A request whose
    /// body cannot be copied is sent once.
    async fn send_retrying(&self, rb: reqwest::RequestBuilder) -> Result<Response> {
        let mut waits = RETRY_WAITS_MS.iter();
        loop {
            let Some(attempt) = rb.try_clone() else { return self.send(rb).await };
            let resp = self.send(attempt).await?;
            if !matches!(resp.status().as_u16(), 500 | 502 | 503 | 504) {
                return Ok(resp);
            }
            let Some(ms) = waits.next() else { return Ok(resp) };
            drop(resp);
            // A little jitter, so two programs that collided do not collide again in step.
            let jitter = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::from(d.subsec_nanos()) % 100);
            tokio::time::sleep(Duration::from_millis(ms + jitter)).await;
        }
    }

    async fn check(resp: Response) -> Result<Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let url = resp.url().to_string();
        let mut body = resp.text().await.unwrap_or_default();
        // At most 500 bytes, cut where a character ends: `truncate` panics when handed an offset
        // inside one, and an error page is exactly where a multibyte character can land on byte 500.
        body.truncate(body.floor_char_boundary(500));
        Err(Error::Status { status, url, body })
    }

    async fn get_json<T: DeserializeOwned>(&self, segments: &[&str], query: &[(&str, String)]) -> Result<T> {
        let resp = self.send_retrying(self.authed(self.http.get(self.url(segments)?).query(query))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    // ---------------------------------------------------------------- auth

    /// Exchange the 8-digit code shown in RomM's web UI (Profile → Client API
    /// Tokens → Pair) for a long-lived `rmm_…` token. Codes are single-use and
    /// expire after 5 minutes.
    pub async fn exchange_pair_code(base_url: &str, code: &str, accept_invalid_certs: bool) -> Result<TokenExchange> {
        let client = Self::new(base_url, None, accept_invalid_certs)?;
        let resp = client
            .headers(client.http.post(client.url(&["client-tokens", "exchange"])?).json(&serde_json::json!({ "code": normalize_pair_code(code) })))
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn heartbeat(&self) -> Result<serde_json::Value> {
        self.get_json(&["heartbeat"], &[]).await
    }

    /// What this server can do. Which save API to use depends on it: a RomM with snapshot sync
    /// (5.5+) keeps saves in channels, and an older one in slots.
    ///
    /// The answer is remembered for a few minutes. A heartbeat that can't be read doesn't change it:
    /// the last answer stands, and with none yet the server is taken to be an older one, whose save
    /// API every RomM still has. So an unreachable or unfamiliar server never gets the newer calls.
    pub async fn capabilities(&self) -> Capabilities {
        let known = self.caps.lock().ok().and_then(|g| g.clone());
        if let Some((at, c)) = &known {
            if at.elapsed() < CAPABILITIES_TTL {
                return c.clone();
            }
        }
        match self.heartbeat().await {
            Ok(v) => {
                let c = Capabilities::from_heartbeat(&v);
                if let Ok(mut g) = self.caps.lock() {
                    *g = Some((Instant::now(), c.clone()));
                }
                c
            }
            Err(_) => known.map(|(_, c)| c).unwrap_or_default(),
        }
    }

    // ------------------------------------------------------------- library

    pub async fn platforms(&self) -> Result<Vec<Platform>> {
        self.get_json(&["platforms"], &[]).await
    }

    pub async fn roms(
        &self,
        platform_id: Option<i64>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Page<Rom>> {
        let mut q: Vec<(&str, String)> = vec![
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("order_by", "name".into()),
            ("with_files", "true".into()),
            // We don't need the filter/char indexes; skipping them keeps big libraries fast.
            ("with_char_index", "false".into()),
            ("with_filter_values", "false".into()),
            ("with_rom_id_index", "false".into()),
        ];
        if let Some(id) = platform_id {
            q.push(("platform_ids", id.to_string()));
        }
        if let Some(s) = search.filter(|s| !s.is_empty()) {
            q.push(("search_term", s.to_string()));
        }
        self.get_json(&["roms"], &q).await
    }

    /// The whole library, one entry per *game*: RomM's `group_by_meta_id` collapses the regional
    /// and revision copies of a title into the one it considers primary (the account's
    /// "default" copy, else the best by the server's region priority; see `versions`), and
    /// lists the rest in that entry's `sibling_roms`.
    ///
    /// Everything is fetched, page by page, so the caller can filter, search and page locally
    /// (this is what Argosy does too). `progress(loaded, total)` is called after every page;
    /// `total` is known from the first page on. Files are left out: only an install needs
    /// them, and it fetches the ROM itself.
    pub async fn library(&self, progress: &mut (dyn FnMut(usize, Option<i64>) + Send)) -> Result<Vec<Rom>> {
        const PAGE: i64 = 500;
        let mut out: Vec<Rom> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let (mut fetched, mut total): (i64, Option<i64>) = (0, None);
        loop {
            let mut q: Vec<(&str, String)> = vec![
                ("limit", PAGE.to_string()),
                ("offset", fetched.to_string()),
                // Name order with RomM's id tiebreak: stable, so paging can't skip or repeat rows.
                ("order_by", "name".into()),
                ("group_by_meta_id", "true".into()),
                ("with_files", "false".into()),
                ("with_char_index", "false".into()),
                ("with_filter_values", "false".into()),
                ("with_rom_id_index", "false".into()),
            ];
            // The count is its own scan; once is enough.
            if fetched > 0 {
                q.push(("with_total", "false".into()));
            }
            let page: Page<Rom> = self.get_json(&["roms"], &q).await?;
            total = total.or(page.total);
            let got = page.items.len() as i64;
            fetched += got;
            // A library edited mid-fetch can shift rows between pages; never list a game twice.
            out.extend(page.items.into_iter().filter(|r| seen.insert(r.id)));
            progress(out.len(), total);
            if got < PAGE || total.is_some_and(|t| fetched >= t) {
                break;
            }
        }
        Ok(out)
    }

    /// Pages through the whole result set.
    pub async fn all_roms(&self, platform_id: Option<i64>, search: Option<&str>) -> Result<Vec<Rom>> {
        const PAGE: i64 = 500;
        let mut out = Vec::new();
        loop {
            let page = self.roms(platform_id, search, PAGE, out.len() as i64).await?;
            let got = page.items.len() as i64;
            out.extend(page.items);
            if got < PAGE {
                break;
            }
        }
        Ok(out)
    }

    pub async fn rom(&self, id: i64) -> Result<Rom> {
        self.get_json(&["roms", &id.to_string()], &[]).await
    }

    /// The size of each of `ids`, in the order given, each once. For the versions of a game:
    /// RomM's `sibling_roms` names them but says nothing about their size, and the server can't
    /// be asked for several ROMs at once, so this is one request each. A ROM that can't be read
    /// (removed meanwhile, a server hiccup) is left out rather than failing the others.
    pub async fn rom_sizes(&self, ids: &[i64]) -> Vec<RomSize> {
        /// More than any game has versions; a bound on what one call can cost.
        const MAX: usize = 32;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for &id in ids.iter().filter(|id| seen.insert(**id)).take(MAX) {
            if let Ok(rom) = self.rom(id).await {
                out.push(RomSize { id, bytes: rom.fs_size_bytes, multi_file: rom.has_multiple_files });
            }
        }
        out
    }

    /// Streams a ROM to `dest`. Single-file ROMs arrive as the file itself;
    /// multi-file ROMs arrive as a zip (RomM includes an .m3u where applicable).
    pub async fn download_rom(
        &self,
        rom: &Rom,
        dest: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        let id = rom.id.to_string();
        let mut url = self.url(&["roms", &id, "content", &rom.fs_name])?;
        let ids: Vec<String> = rom.files.iter().map(|f| f.id.to_string()).collect();
        if rom.has_multiple_files && !ids.is_empty() {
            url.query_pairs_mut().append_pair("file_ids", &ids.join(","));
        }
        self.download_url(url, dest, progress).await
    }

    pub async fn download_firmware(
        &self,
        fw: &Firmware,
        dest: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        let url = self.url(&["firmware", &fw.id.to_string(), "content", &fw.file_name])?;
        self.download_url(url, dest, progress).await
    }

    /// Download to `<dest>.part`, then rename, so an interrupted download never
    /// leaves a truncated file under the real name.
    async fn download_url(&self, url: Url, dest: &Path, progress: &mut (dyn FnMut(u64, Option<u64>) + Send)) -> Result<()> {
        let resp = Self::check(self.send_retrying(self.authed(self.http.get(url))).await?).await?;
        let total = resp.content_length();
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut part = dest.as_os_str().to_owned();
        part.push(".part");
        let part = std::path::PathBuf::from(part);

        let result: Result<()> = async {
            let mut file = tokio::fs::File::create(&part).await?;
            let mut done = 0u64;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                done += chunk.len() as u64;
                progress(done, total);
            }
            file.flush().await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                tokio::fs::rename(&part, dest).await?;
                Ok(())
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(e)
            }
        }
    }

    // --------------------------------------------------------------- sync

    /// Idempotent: RomM returns the existing device when `hostname`+`platform` match.
    pub async fn register_device(&self, reg: &DeviceRegistration) -> Result<Device> {
        let resp = self.send_retrying(self.authed(self.http.post(self.url(&["devices"])?).json(reg))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn negotiate(&self, req: &NegotiateRequest) -> Result<NegotiateResponse> {
        let resp = self.send_retrying(self.authed(self.http.post(self.url(&["sync", "negotiate"])?).json(req))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn complete_session(&self, session_id: i64, req: &CompleteRequest) -> Result<()> {
        let url = self.url(&["sync", "sessions", &session_id.to_string(), "complete"])?;
        let resp = self.send_retrying(self.authed(self.http.post(url).json(req))).await?;
        Self::check(resp).await?;
        Ok(())
    }

    fn upload_query(p: &UploadParams) -> Vec<(&'static str, String)> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(e) = &p.emulator {
            q.push(("emulator", e.clone()));
        }
        if let Some(s) = &p.slot {
            q.push(("slot", s.clone()));
        }
        if let Some(d) = &p.device_id {
            q.push(("device_id", d.clone()));
        }
        if let Some(s) = p.session_id {
            q.push(("session_id", s.to_string()));
        }
        if let Some(h) = &p.content_hash {
            q.push(("content_hash", h.clone()));
        }
        q
    }

    /// New save. `overwrite=false`, so a slot someone else filled returns 409
    /// instead of being clobbered; callers should treat 409 as a conflict.
    pub async fn upload_save(&self, p: &UploadParams, bytes: Vec<u8>) -> Result<Save> {
        let mut q = Self::upload_query(p);
        q.push(("rom_id", p.rom_id.to_string()));
        q.push(("overwrite", "false".into()));
        q.push(("autocleanup", "true".into()));
        q.push(("autocleanup_limit", "10".into()));
        let size = bytes.len() as u64;
        let part = multipart::Part::bytes(bytes).file_name(p.file_name.clone());
        let form = multipart::Form::new().part("saveFile", part);
        let resp = self
            .send_upload(self.authed(self.http.post(self.url(&["saves"])?).query(&q).multipart(form)), size)
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Replace a save this device created. RomM applies no conflict guard here.
    pub async fn replace_save(&self, save_id: i64, p: &UploadParams, bytes: Vec<u8>) -> Result<Save> {
        let q = Self::upload_query(p);
        let size = bytes.len() as u64;
        let part = multipart::Part::bytes(bytes).file_name(p.file_name.clone());
        let form = multipart::Form::new().part("saveFile", part);
        let resp = self
            .send_upload(self.authed(self.http.put(self.url(&["saves", &save_id.to_string()])?).query(&q).multipart(form)), size)
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// The URL of a server-relative path (`/assets/...`, `/api/saves/3/content?...`) as the server named it.
    fn server_relative(&self, path: &str) -> Result<Url> {
        let rel = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        Url::parse(&format!("{}{}", self.base, rel)).map_err(|e| Error::msg(e.to_string()))
    }

    /// Fetch a server-relative resource such as cover art (`/assets/romm/resources/...`).
    ///
    /// Only for what nginx serves itself: it skips the turn-taking `send` does, which is there for requests
    /// that reach RomM's auth middleware. A path under `/api` must not come through here; use `send_retrying`.
    pub async fn fetch_asset(&self, path: &str) -> Result<(Vec<u8>, Option<String>)> {
        let url = self.server_relative(path)?;
        let resp = Self::check(self.headers(self.authed(self.http.get(url))).await?).await?;
        let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
        Ok((resp.bytes().await?.to_vec(), ctype))
    }

    // ------------------------------------------------- channels and snapshots (RomM 5.5+)

    /// The caller's channels on these ROM files, empty ones included, each with its current snapshot.
    pub async fn channels(&self, rom_file_ids: &[i64]) -> Result<Vec<Channel>> {
        let q: Vec<(&str, String)> = rom_file_ids.iter().map(|id| ("rom_file_id", id.to_string())).collect();
        self.get_json(&["channels"], &q).await
    }

    /// One snapshot. With `hold`, the server records that this device now has it, which is how a
    /// later push without an explicit parent knows what the device built on.
    pub async fn snapshot(&self, id: i64, device_id: &str, hold: bool) -> Result<Snapshot> {
        let mut q = vec![("device_id", device_id.to_string())];
        if hold {
            q.push(("hold", "true".into()));
        }
        self.get_json(&["snapshots", &id.to_string()], &q).await
    }

    /// Writes a snapshot into a channel: the `manifest`, plus the save's bytes when the server doesn't
    /// have them yet. The server answers in more ways than success and failure, and a client has to
    /// act on each; see `PushOutcome`.
    pub async fn push_snapshot(&self, device_id: &str, manifest: &Manifest, save: Option<(String, Vec<u8>)>) -> Result<PushOutcome> {
        let json = serde_json::to_string(manifest)?;
        let mut size = json.len() as u64;
        let mut form = multipart::Form::new().part("manifest", multipart::Part::text(json).mime_str("application/json").map_err(Error::Http)?);
        if let Some((name, bytes)) = save {
            size += bytes.len() as u64;
            form = form.part("save", multipart::Part::bytes(bytes).file_name(name).mime_str("application/octet-stream").map_err(Error::Http)?);
        }
        let url = self.url(&["snapshots"])?;
        let resp = self.send_upload(self.authed(self.http.post(url).query(&[("device_id", device_id)]).multipart(form)), size).await?;
        let status = resp.status().as_u16();
        match status {
            200 | 201 => Ok(PushOutcome::Written { snapshot: resp.json().await?, created: status == 201 }),
            409 => {
                let conflict: PushConflict = resp.json().await.unwrap_or_default();
                Ok(if conflict.hardcore_downgrade { PushOutcome::HardcoreDowngrade } else { PushOutcome::Conflict(Box::new(conflict)) })
            }
            400 | 404 => {
                // Both carry a body that says what to do next; anything else about them is an error.
                let url = resp.url().to_string();
                let body = resp.text().await.unwrap_or_default();
                if status == 400 {
                    #[derive(serde::Deserialize)]
                    struct Missing {
                        #[serde(default)]
                        missing: Vec<String>,
                    }
                    if let Some(m) = serde_json::from_str::<Missing>(&body).ok().filter(|m| !m.missing.is_empty()) {
                        return Ok(PushOutcome::Missing(m.missing));
                    }
                } else if body.contains("Parent snapshot not found") {
                    return Ok(PushOutcome::ParentMissing);
                }
                Err(Error::Status { status, url, body: body.chars().take(500).collect() })
            }
            _ => Err(Self::check(resp).await.err().unwrap_or_else(|| Error::msg(format!("unexpected answer {status} to a snapshot push")))),
        }
    }

    /// The bytes of a snapshot's save, from the server-relative `download_path` it names.
    ///
    /// That path is `/api/saves/{id}/content?...`, an ordinary authenticated API route, so it takes its turn
    /// and is retried like any other read (see `send`): going round the gate let it race another request's
    /// auth step and fail with the 500 the gate exists to prevent, with nothing to retry it.
    pub async fn download_snapshot_save(&self, download_path: &str) -> Result<Vec<u8>> {
        let url = self.server_relative(download_path)?;
        let resp = Self::check(self.send_retrying(self.authed(self.http.get(url))).await?).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// Reports play sessions on their own, which RomM 5.5 takes apart from a sync session (snapshot sync
    /// has none, and the older way of reporting them rode on one).
    pub async fn ingest_play_sessions(&self, device_id: &str, sessions: &[PlaySession]) -> Result<()> {
        let body = serde_json::json!({ "device_id": device_id, "sessions": sessions });
        let resp = self.send_retrying(self.authed(self.http.post(self.url(&["play-sessions"])?).json(&body))).await?;
        Self::check(resp).await?;
        Ok(())
    }

    // ------------------------------------------------------------- states

    pub async fn list_states(&self, rom_id: i64) -> Result<Vec<StateInfo>> {
        self.get_json(&["states"], &[("rom_id", rom_id.to_string())]).await
    }

    /// Create or replace a state. Without `existing_id` this is `POST /api/states`, which RomM
    /// treats as an upsert on (user, ROM, file name). With it, `PUT /api/states/{id}`.
    /// The file is streamed from disk rather than read into memory (states can be 100+ MB).
    pub async fn upload_state(
        &self,
        rom_id: i64,
        emulator: &str,
        existing_id: Option<i64>,
        name: &str,
        path: &Path,
        screenshot: Option<&Path>,
    ) -> Result<StateInfo> {
        async fn file_part(path: &Path, name: &str) -> Result<multipart::Part> {
            let file = tokio::fs::File::open(path).await?;
            let len = file.metadata().await?.len();
            let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
            Ok(multipart::Part::stream_with_length(body, len).file_name(name.to_string()))
        }
        let mut size = tokio::fs::metadata(path).await?.len();
        let mut form = multipart::Form::new().part("stateFile", file_part(path, name).await?);
        if let Some(png) = screenshot {
            let shot_name = format!("{name}.png");
            size += tokio::fs::metadata(png).await?.len();
            form = form.part("screenshotFile", file_part(png, &shot_name).await?);
        }
        let rb = match existing_id {
            Some(id) => self.http.put(self.url(&["states", &id.to_string()])?),
            None => self
                .http
                .post(self.url(&["states"])?)
                .query(&[("rom_id", rom_id.to_string()), ("emulator", emulator.to_string())]),
        };
        let resp = self.send_upload(self.authed(rb.multipart(form)), size).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn download_state(
        &self,
        id: i64,
        dest: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        let url = self.url(&["states", &id.to_string(), "content"])?;
        self.download_url(url, dest, progress).await
    }

    pub async fn delete_states(&self, ids: &[i64]) -> Result<()> {
        let resp = self
            .send(self.authed(self.http.post(self.url(&["states", "delete"])?).json(&serde_json::json!({ "states": ids }))))
            .await?;
        Self::check(resp).await?;
        Ok(())
    }

    // ------------------------------------------------------ RetroAchievements

    /// The signed-in user (`me.read`). This carries the user's *entire* RetroAchievements
    /// progression, which is large for heavy RA users, so callers should cache it.
    pub async fn me(&self) -> Result<Me> {
        self.get_json(&["users", "me"], &[]).await
    }

    /// Only the achievement fields of one ROM (the full ROM response is much bigger).
    pub async fn rom_ra(&self, id: i64) -> Result<RomRa> {
        self.get_json(&["roms", &id.to_string()], &[]).await
    }

    /// Ask RomM to re-pull the user's progress from retroachievements.org.
    ///
    /// Needs the `me.write` scope, which the app doesn't otherwise request (it also allows editing
    /// the profile), so a 403 here is expected for most tokens. `incremental` only re-fetches games
    /// whose award count or last-earned date changed, which is what you want after playing.
    pub async fn refresh_ra(&self, user_id: i64, incremental: bool) -> Result<()> {
        let url = self.url(&["users", &user_id.to_string(), "ra", "refresh"])?;
        let resp = self.send(self.authed(self.http.post(url).json(&serde_json::json!({ "incremental": incremental })))).await?;
        Self::check(resp).await?;
        Ok(())
    }

    pub async fn download_save(&self, save_id: i64, device_id: &str, session_id: i64) -> Result<Vec<u8>> {
        let q = [
            ("device_id", device_id.to_string()),
            ("session_id", session_id.to_string()),
            ("optimistic", "true".to_string()),
        ];
        let url = self.url(&["saves", &save_id.to_string(), "content"])?;
        let resp = self.send_retrying(self.authed(self.http.get(url).query(&q))).await?;
        Ok(Self::check(resp).await?.bytes().await?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_code_is_letters_and_digits_in_upper_case() {
        assert_eq!(normalize_pair_code(" 1234-5678 "), "12345678");
        assert_eq!(normalize_pair_code("ab cd"), "ABCD");
        assert_eq!(normalize_pair_code("12345678"), "12345678");
    }

    fn granted(scopes: &[&str]) -> Vec<String> {
        scopes.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_token_with_every_scope_lacks_none() {
        assert!(missing_scopes(&granted(REQUIRED_SCOPES)).is_empty());
    }

    #[test]
    fn extra_scopes_are_harmless_and_missing_ones_are_named_in_the_required_order() {
        let mut have: Vec<&str> = REQUIRED_SCOPES.iter().copied().filter(|s| *s != "assets.write" && *s != "me.read").collect();
        have.push("me.write");
        assert_eq!(missing_scopes(&granted(&have)), ["me.read", "assets.write"]);
    }

    #[test]
    fn a_token_with_no_scopes_lacks_them_all() {
        assert_eq!(missing_scopes(&[]), REQUIRED_SCOPES);
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(normalize_base_url("192.168.1.10:8080").unwrap(), "http://192.168.1.10:8080");
        assert_eq!(normalize_base_url("https://romm.example.com/").unwrap(), "https://romm.example.com");
        assert!(normalize_base_url("  ").is_err());
    }

    #[test]
    fn a_bare_address_gets_https_unless_it_is_on_the_users_own_network() {
        // The sign-in sends a token: a public name must not go out as plain http by default.
        for public in ["romm.example.com", "romm.example.com:8443", "romm.example.com/romm", "8.8.8.8:8080", "[2001:db8::1]:8080", "100.63.0.1", "172.32.0.1", "romm.local.example.com"] {
            assert!(normalize_base_url(public).unwrap().starts_with("https://"), "{public}");
        }
        // The usual self-hosted setup keeps working without typing a scheme.
        for local in [
            "192.168.1.10:8080", "10.0.0.5", "172.16.3.4", "127.0.0.1:8080", "localhost:8080", "[::1]:8080", "[fd00::5]:80", "[fe80::1]:80",
            "100.64.0.9:8080", "100.127.255.1", "169.254.1.1", "romm", "nas:8080", "romm.local", "romm.lan:8080", "ROMM.Home.Arpa", "romm.internal", "romm.", "[::ffff:192.168.1.1]:80",
        ] {
            assert!(normalize_base_url(local).unwrap().starts_with("http://"), "{local}");
        }
    }

    #[test]
    fn a_scheme_that_was_typed_is_never_changed() {
        assert_eq!(normalize_base_url("http://romm.example.com").unwrap(), "http://romm.example.com");
        assert_eq!(normalize_base_url("https://192.168.1.10:8080/").unwrap(), "https://192.168.1.10:8080");
        assert!(normalize_base_url("exa mple.com").is_err());
    }

    #[test]
    fn plain_http_to_somewhere_that_is_not_local_is_warned_about_but_nothing_else_is() {
        let w = cleartext_warning("http://romm.example.com").expect("a public host over http");
        assert!(w.contains("romm.example.com") && w.contains("https://"), "{w}");
        assert!(cleartext_warning("https://romm.example.com").is_none());
        assert!(cleartext_warning("http://192.168.1.10:8080").is_none());
        assert!(cleartext_warning("http://romm.lan").is_none());
        assert!(cleartext_warning("not a url").is_none());
    }

    #[test]
    fn an_upload_may_take_longer_the_bigger_it_is() {
        assert_eq!(upload_limit(0), UPLOAD_BASE_LIMIT);
        assert_eq!(upload_limit(5_000_000), UPLOAD_BASE_LIMIT + Duration::from_secs(100));
        // A 512 MiB state on a very slow line (50 KB/s) still has hours; it is not cut off after a minute.
        assert!(upload_limit(512 << 20) > Duration::from_secs(2 * 3600));
    }

    #[test]
    fn encodes_path_segments() {
        let c = RommClient::new("http://h", None, false).unwrap();
        let u = c.url(&["roms", "1", "content", "Zelda: A Link/Between #1.zip"]).unwrap();
        assert!(u.as_str().ends_with("/api/roms/1/content/Zelda:%20A%20Link%2FBetween%20%231.zip"), "{u}");
    }

    #[test]
    fn keeps_base_path_prefix() {
        let c = RommClient::new("https://example.com/romm", None, false).unwrap();
        assert_eq!(c.url(&["platforms"]).unwrap().as_str(), "https://example.com/romm/api/platforms");
    }
}
