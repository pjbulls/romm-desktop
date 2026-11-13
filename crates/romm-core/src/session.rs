//! One play session: pull saves → run the emulator → push.
//!
//! Network trouble never blocks playing and never loses data: if the pre-launch pull fails you
//! still play, and if the post-session push fails nothing is marked as synced, so the next sync
//! finds the save still changed and uploads it. (Which files are a game's save is worked out from
//! the game's id each time, by `unit`; nothing is attributed by watching folders around a session.
//! Only save *states* in shared folders still are.)
//!
//! The session is split into `pre_play`, `run_emulator` and `post_play` so a GUI can
//! hold its database lock only during the two short sync phases, not for the hours
//! an emulator might run. `play` composes them for the CLI.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use tokio::process::Command;

use crate::api::types::PlaySession;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::saves::{self, StatSnapshot, SLOT};
use crate::state::{LocalRom, State};
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
    pub changed_states: Vec<String>,
}

/// Everything `pre_play` learned, handed to `post_play`.
pub struct Pre {
    pub pull: Option<SyncReport>,
    pub pull_error: Option<String>,
    pub state_pull: Option<StateReport>,
    pub state_error: Option<String>,
    before_states: Option<StatSnapshot>,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub started: SystemTime,
    pub ended: SystemTime,
}

fn leaf(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Everything one game's sync needs besides the game: who to talk to and as which device, where
/// backups go, how a conflict is settled, and whether states are synced along with the saves.
#[derive(Clone, Copy)]
pub struct SyncCtx<'a> {
    pub client: &'a RommClient,
    pub device_id: &'a str,
    pub backup_dir: &'a Path,
    pub policy: ConflictPolicy,
    /// `None` leaves save states alone.
    pub states: Option<&'a StateOptions>,
}

/// What one round of syncing a game did: its saves, then its states. A failure is kept, not
/// raised, and one failing does not stop the other from being tried; what a failure means (a
/// note, an exit code, a failed job) is for the caller to say.
#[derive(Debug)]
pub struct GameSync {
    pub saves: Result<SyncReport>,
    /// `None` when states were not synced.
    pub states: Option<Result<StateReport>>,
}

impl SyncCtx<'_> {
    /// Syncs `rom`'s saves, then its states when `self.states` is set. `play` is recorded with the
    /// server when the saves are synced. The caller saves `state` afterwards.
    pub async fn sync_game(&self, state: &mut State, rom: &LocalRom, env: &EmuEnv, play: Option<PlaySession>) -> GameSync {
        let saves = {
            let mut s = Syncer { client: self.client, device_id: self.device_id, state: &mut *state, backup_dir: self.backup_dir.to_path_buf(), policy: self.policy };
            s.sync_rom(rom, env, play).await
        };
        let states = match self.states {
            Some(opts) => Some(states::sync_states(self.client, state, self.backup_dir, rom, env, self.policy, opts).await),
            None => None,
        };
        GameSync { saves, states }
    }
}

/// One round of syncing with each failure turned into the message that goes with it: what the
/// two sync phases of a session keep, whichever phase it was.
struct Folded {
    saves: Option<SyncReport>,
    saves_error: Option<String>,
    states: Option<StateReport>,
    states_error: Option<String>,
}

impl From<GameSync> for Folded {
    fn from(round: GameSync) -> Folded {
        let (saves, saves_error) = match round.saves {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let (states, states_error) = match round.states {
            Some(Ok(r)) => (Some(r), None),
            Some(Err(e)) => (None, Some(e.to_string())),
            None => (None, None),
        };
        Folded { saves, saves_error, states, states_error }
    }
}

/// Phase 1: pull saves (and states), then snapshot the shared state folder. Failures to reach the
/// server are recorded, not raised.
pub async fn pre_play(cx: &SyncCtx<'_>, state: &mut State, state_path: &Path, rom: &LocalRom, env: &EmuEnv) -> Result<Pre> {
    let round = Folded::from(cx.sync_game(state, rom, env, None).await);
    let mut pre = Pre { pull: round.saves, pull_error: round.saves_error, state_pull: round.states, state_error: round.states_error, before_states: None };
    state.save(state_path)?;

    if cx.states.is_some() {
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

/// Phase 3: attribute changed states, then push saves, states and playtime.
pub async fn post_play(cx: &SyncCtx<'_>, state: &mut State, state_path: &Path, rom: &LocalRom, env: &EmuEnv, pre: Pre, run: &Run) -> Result<PlayOutcome> {
    let mut out = PlayOutcome {
        pull: pre.pull,
        pull_error: pre.pull_error,
        state_pull: pre.state_pull,
        state_error: pre.state_error,
        exit_code: run.exit_code,
        elapsed: run.elapsed,
        ..Default::default()
    };

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
    let round = Folded::from(cx.sync_game(state, rom, env, Some(play)).await);
    out.push = round.saves;
    out.push_error = round.saves_error;
    out.state_push = round.states;
    // A state push that failed replaces what the pull said about states; one that went well leaves it.
    if round.states_error.is_some() {
        out.state_error = round.states_error;
    }
    state.save(state_path)?;
    Ok(out)
}

/// pull → run → push, for callers that can hold `state` for the whole session (the CLI).
/// `before_launch` runs once the pull is done, just before the emulator starts.
pub async fn play(cx: &SyncCtx<'_>, state: &mut State, state_path: &Path, rom: &LocalRom, env: &EmuEnv, cmd: Command, before_launch: impl FnOnce()) -> Result<PlayOutcome> {
    let pre = pre_play(cx, state, state_path, rom, env).await?;
    before_launch();
    let run = run_emulator(cmd, &env.profile.name).await?;
    post_play(cx, state, state_path, rom, env, pre, &run).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(uploaded: &str) -> SyncReport {
        SyncReport { uploaded: vec![uploaded.into()], ..Default::default() }
    }

    #[test]
    fn a_failure_is_folded_into_its_message_and_a_success_into_its_report() {
        let f = Folded::from(GameSync { saves: Err(Error::msg("offline")), states: Some(Ok(report("x.state1"))) });
        assert!(f.saves.is_none());
        assert_eq!(f.saves_error.as_deref(), Some("offline"));
        assert_eq!(f.states.map(|r| r.uploaded), Some(vec!["x.state1".to_string()]));
        assert!(f.states_error.is_none());

        let f = Folded::from(GameSync { saves: Ok(report("a.srm")), states: Some(Err(Error::msg("too big"))) });
        assert_eq!(f.saves.map(|r| r.uploaded), Some(vec!["a.srm".to_string()]));
        assert!(f.saves_error.is_none());
        assert!(f.states.is_none());
        assert_eq!(f.states_error.as_deref(), Some("too big"));
    }

    #[test]
    fn states_that_were_not_synced_are_neither_a_report_nor_an_error() {
        let f = Folded::from(GameSync { saves: Ok(SyncReport::default()), states: None });
        assert!(f.states.is_none() && f.states_error.is_none());
    }
}
