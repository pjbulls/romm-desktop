//! One play session: pull saves → run the emulator → attribute what changed → push.
//!
//! Network trouble never blocks playing and never loses data: if the pre-launch
//! pull fails you still play, and if the post-session push fails the changed files
//! stay tracked in the state file so the next sync uploads them.
//!
//! The session is split into `pre_play`, `run_emulator` and `post_play` so a GUI can
//! hold its database lock only during the two short sync phases, not for the hours
//! an emulator might run. `play` composes them for the CLI.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use tokio::process::Command;

use crate::api::types::PlaySession;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::profiles::SaveMode;
use crate::saves::{self, StatSnapshot, Snapshot, SLOT};
use crate::state::{save_key, LocalRom, State};
use crate::states::{self, StateOptions, StateReport};
use crate::sync::{ConflictPolicy, EmuEnv, SyncReport, Syncer};

#[derive(Debug, Default)]
pub struct PlayOutcome {
    pub pull: Option<SyncReport>,
    pub pull_error: Option<String>,
    pub push: Option<SyncReport>,
    pub push_error: Option<String>,
    pub state_pull: Option<StateReport>,
    pub state_push: Option<StateReport>,
    pub state_error: Option<String>,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    /// Files attributed to this game during the session (shared-folder emulators).
    pub changed_files: Vec<String>,
    pub changed_states: Vec<String>,
}

/// Everything `pre_play` learned, handed to `post_play`.
pub struct Pre {
    pub pull: Option<SyncReport>,
    pub pull_error: Option<String>,
    pub state_pull: Option<StateReport>,
    pub state_error: Option<String>,
    before: Option<Snapshot>,
    before_states: Option<StatSnapshot>,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub started: SystemTime,
    pub ended: SystemTime,
}

/// Remember which files this game touched (union with those seen in earlier sessions).
pub fn attribute_changes(state: &mut State, rom_id: i64, emulator: &str, changed: &[String]) {
    if changed.is_empty() {
        return;
    }
    let rec = state.saves.entry(save_key(rom_id, emulator)).or_default();
    let mut files: Vec<String> = rec.files.iter().cloned().chain(changed.iter().cloned()).collect();
    files.sort();
    files.dedup();
    rec.files = files;
}

fn leaf(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Phase 1: pull saves (and states), then snapshot shared folders. Failures to reach the server are
/// recorded, not raised.
#[allow(clippy::too_many_arguments)]
pub async fn pre_play(
    client: &RommClient,
    device_id: &str,
    state: &mut State,
    state_path: &Path,
    backup_dir: PathBuf,
    rom: &LocalRom,
    env: &EmuEnv,
    policy: ConflictPolicy,
    states: Option<&StateOptions>,
) -> Result<Pre> {
    let mut pre = Pre { pull: None, pull_error: None, state_pull: None, state_error: None, before: None, before_states: None };
    {
        let mut s = Syncer { client, device_id, state: &mut *state, backup_dir: backup_dir.clone(), policy };
        match s.sync_rom(rom, env, None).await {
            Ok(r) => pre.pull = Some(r),
            Err(e) => pre.pull_error = Some(e.to_string()),
        }
    }
    if let Some(opts) = states {
        match states::sync_states(client, state, &backup_dir, rom, env, policy, opts).await {
            Ok(r) => pre.state_pull = Some(r),
            Err(e) => pre.state_error = Some(e.to_string()),
        }
    }
    state.save(state_path)?;

    if env.save_mode() == Some(SaveMode::Diff) {
        pre.before = Some(saves::snapshot(&env.data_root, &env.watch_roots())?);
    }
    if states.is_some() {
        let roots = env.state_watch_roots();
        if !roots.is_empty() {
            pre.before_states = Some(saves::snapshot_stat(&env.data_root, &roots)?);
        }
    }
    Ok(pre)
}

/// Phase 2: run the emulator to completion. Touches no state, so no lock is needed.
pub async fn run_emulator(mut cmd: Command, emulator_name: &str) -> Result<Run> {
    let started = SystemTime::now();
    let t0 = Instant::now();
    let status = cmd
        .status()
        .await
        .map_err(|e| Error::msg(format!("could not start {emulator_name}: {e}")))?;
    Ok(Run { exit_code: status.code(), elapsed: t0.elapsed(), started, ended: SystemTime::now() })
}

/// Phase 3: attribute what changed, then push saves, states and playtime.
#[allow(clippy::too_many_arguments)]
pub async fn post_play(
    client: &RommClient,
    device_id: &str,
    state: &mut State,
    state_path: &Path,
    backup_dir: PathBuf,
    rom: &LocalRom,
    env: &EmuEnv,
    policy: ConflictPolicy,
    states: Option<&StateOptions>,
    pre: Pre,
    run: &Run,
) -> Result<PlayOutcome> {
    let mut out = PlayOutcome {
        pull: pre.pull,
        pull_error: pre.pull_error,
        state_pull: pre.state_pull,
        state_error: pre.state_error,
        exit_code: run.exit_code,
        elapsed: run.elapsed,
        ..Default::default()
    };

    if let Some(before) = pre.before {
        let after = saves::snapshot(&env.data_root, &env.watch_roots())?;
        out.changed_files = saves::changed_files(&before, &after);
        attribute_changes(state, rom.rom_id, &env.profile.id, &out.changed_files);
    }
    if let Some(before) = pre.before_states {
        let after = saves::snapshot_stat(&env.data_root, &env.state_watch_roots())?;
        out.changed_states = saves::changed_stat(&before, &after)
            .iter()
            .map(|r| leaf(r).to_string())
            .filter(|n| !env.state_ignored(n))
            .collect();
        state.attribute_states(rom.rom_id, &env.profile.id, &out.changed_states);
    }
    state.save(state_path)?;

    let play = PlaySession {
        rom_id: rom.rom_id,
        save_slot: Some(SLOT.to_string()),
        start_time: saves::iso(run.started),
        end_time: saves::iso(run.ended.max(run.started + Duration::from_secs(1))),
        duration_ms: run.elapsed.as_millis() as u64,
    };
    {
        let mut s = Syncer { client, device_id, state: &mut *state, backup_dir: backup_dir.clone(), policy };
        match s.sync_rom(rom, env, Some(play)).await {
            Ok(r) => out.push = Some(r),
            Err(e) => out.push_error = Some(e.to_string()),
        }
    }
    if let Some(opts) = states {
        match states::sync_states(client, state, &backup_dir, rom, env, policy, opts).await {
            Ok(r) => out.state_push = Some(r),
            Err(e) => out.state_error = Some(e.to_string()),
        }
    }
    state.save(state_path)?;
    Ok(out)
}

/// pull → run → push, for callers that can hold `state` for the whole session (the CLI).
#[allow(clippy::too_many_arguments)]
pub async fn play(
    client: &RommClient,
    device_id: &str,
    state: &mut State,
    state_path: &Path,
    backup_dir: PathBuf,
    rom: &LocalRom,
    env: &EmuEnv,
    cmd: Command,
    policy: ConflictPolicy,
    states: Option<&StateOptions>,
) -> Result<PlayOutcome> {
    let pre = pre_play(client, device_id, state, state_path, backup_dir.clone(), rom, env, policy, states).await?;
    let run = run_emulator(cmd, &env.profile.name).await?;
    post_play(client, device_id, state, state_path, backup_dir, rom, env, policy, states, pre, &run).await
}
