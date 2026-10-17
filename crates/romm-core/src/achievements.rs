//! RetroAchievements: merge a game's achievement set with what the user has earned.
//!
//! Pure functions over the two RomM responses (no I/O), so the matching rules can be tested
//! without a server. Where the data comes from is documented in `api/types.rs`.
//!
//! ## How earned achievements are matched
//!
//! RomM stores an earned achievement as `{ id: <badge name>, date, date_hardcore }`. The `id` is
//! RetroAchievements' *badge name* (e.g. `"250336"`), **not** the achievement's numeric id, so the
//! match is against `RaAchievement::badge_id`. Matching on `ra_id` would show nothing as unlocked.
//!
//! Badge names are not guaranteed unique within a game: RomM itself notes that achievements can
//! share a badge image. When several achievements share one, the earned list holds one entry per
//! *earned* achievement with that badge, so we hand out the entries in display order. The count is
//! always right; which of the look-alikes is marked is a best guess, because the data can't say.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::api::types::{Me, RaAchievement, RaEarned, RomRa};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaState {
    /// A set exists and the user has a RetroAchievements account linked in RomM.
    Ready,
    /// RomM has no achievement set for this game (not matched to a RetroAchievements game, or
    /// RetroAchievements has none). Nothing to show.
    NoSet,
    /// The set exists, but the user hasn't entered a RetroAchievements username in RomM, so we
    /// can list the achievements but not which are earned.
    NotLinked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AchievementView {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub points: i64,
    /// `progression`, `win_condition`, `missable`, or empty.
    pub kind: String,
    pub earned: bool,
    /// Earned in hardcore mode.
    pub hardcore: bool,
    /// RFC 3339 (UTC) when the achievement was earned.
    pub earned_at: Option<String>,
    /// Server path of the image to show: the coloured badge if earned, the greyed one if not.
    pub badge: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AchievementsView {
    pub state: RaState,
    pub ra_username: Option<String>,
    pub total: u32,
    pub earned: u32,
    pub earned_hardcore: u32,
    pub points_total: u32,
    pub points_earned: u32,
    /// `completed`, `mastered`, `beaten-softcore`, `beaten-hardcore`, or `None`.
    pub award: Option<String>,
    /// When the most recent achievement was earned (RFC 3339).
    pub last_earned: Option<String>,
    /// RomM holds no progression for this user at all, so "0 earned" may only mean "never
    /// fetched". A user with progression who simply hasn't played this game gets `false`.
    pub progress_missing: bool,
    pub achievements: Vec<AchievementView>,
    /// Set by the app when this came from the on-disk copy because the server was unreachable.
    #[serde(default)]
    pub stale: bool,
    /// When the app last got this from the server (RFC 3339). Set by the app.
    #[serde(default)]
    pub fetched_at: Option<String>,
}

/// RetroAchievements timestamps come as `2026-01-02 03:04:05` (UTC), sometimes date-only, and
/// RomM passes them through untouched. WebKit's `Date.parse` rejects the space-separated form, so
/// the UI would show raw text. Normalise to RFC 3339; anything unrecognised is returned as-is.
pub fn normalize_ts(s: &str) -> String {
    let s = s.trim();
    const OUT: &str = "%Y-%m-%dT%H:%M:%SZ";
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.naive_utc().format(OUT).to_string();
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return dt.format(OUT).to_string();
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            return dt.format(OUT).to_string();
        }
    }
    s.to_string()
}

/// The current time as RFC 3339 (UTC), for `AchievementsView::fetched_at`.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// RomM already sorts by RetroAchievements' "Display Order", but don't depend on it.
/// Missing values sort last; ties fall back to the achievement id.
fn sorted_catalog(rom: &RomRa) -> Vec<&RaAchievement> {
    let mut list: Vec<&RaAchievement> = rom.merged_ra_metadata.iter().flat_map(|m| m.achievements.iter()).collect();
    list.sort_by_key(|a| (a.display_order.unwrap_or(i64::MAX), a.ra_id.unwrap_or(i64::MAX)));
    list
}

pub fn build(rom: &RomRa, me: &Me) -> AchievementsView {
    let catalog = sorted_catalog(rom);

    let progress = rom.ra_id.and_then(|ra_id| {
        me.ra_progression.as_ref()?.results.iter().find(|r| r.rom_ra_id == Some(ra_id))
    });

    // badge name -> earned entries still unclaimed (see module docs on shared badges).
    let mut unclaimed: HashMap<&str, Vec<&RaEarned>> = HashMap::new();
    if let Some(p) = progress {
        for e in &p.earned_achievements {
            unclaimed.entry(e.id.as_str()).or_default().push(e);
        }
        for list in unclaimed.values_mut() {
            list.reverse(); // so pop() yields them in original order
        }
    }

    let achievements: Vec<AchievementView> = catalog
        .iter()
        .map(|a| {
            let hit = a.badge_id.as_deref().filter(|b| !b.is_empty()).and_then(|b| unclaimed.get_mut(b)?.pop());
            let hardcore = hit.is_some_and(|e| e.date_hardcore.is_some());
            let earned_at = hit.and_then(|e| e.date_hardcore.as_deref().or(e.date.as_deref())).map(normalize_ts);
            let earned = hit.is_some();
            AchievementView {
                id: a.ra_id.unwrap_or(0),
                title: a.title.clone().unwrap_or_default(),
                description: a.description.clone().unwrap_or_default(),
                points: a.points.unwrap_or(0),
                kind: a.kind.clone().unwrap_or_default(),
                earned,
                hardcore,
                earned_at,
                badge: if earned { a.badge_path.clone() } else { a.badge_path_lock.clone().or_else(|| a.badge_path.clone()) },
            }
        })
        .collect();

    let state = if achievements.is_empty() {
        RaState::NoSet
    } else if me.ra_username.as_deref().map_or(true, |u| u.trim().is_empty()) {
        RaState::NotLinked
    } else {
        RaState::Ready
    };

    let count = |f: &dyn Fn(&AchievementView) -> bool| achievements.iter().filter(|a| f(a)).count() as u32;
    let points = |f: &dyn Fn(&AchievementView) -> bool| achievements.iter().filter(|a| f(a)).map(|a| a.points.max(0) as u32).sum::<u32>();

    AchievementsView {
        state,
        ra_username: me.ra_username.clone().filter(|u| !u.trim().is_empty()),
        total: achievements.len() as u32,
        earned: count(&|a| a.earned),
        earned_hardcore: count(&|a| a.hardcore),
        points_total: points(&|_| true),
        points_earned: points(&|a| a.earned),
        award: progress.and_then(|p| p.highest_award_kind.clone()),
        last_earned: achievements.iter().filter_map(|a| a.earned_at.clone()).max(),
        progress_missing: me.ra_progression.is_none(),
        achievements,
        stale: false,
        fetched_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn rom(v: Value) -> RomRa {
        serde_json::from_value(v).unwrap()
    }
    fn me(v: Value) -> Me {
        serde_json::from_value(v).unwrap()
    }
    fn ach(ra_id: i64, badge: &str, order: i64, points: i64) -> Value {
        json!({
            "ra_id": ra_id, "title": format!("Ach {ra_id}"), "description": "do it", "points": points,
            "badge_id": badge, "badge_path": format!("/assets/romm/resources/b/{badge}.png"),
            "badge_path_lock": format!("/assets/romm/resources/b/{badge}_lock.png"),
            "display_order": order, "type": ""
        })
    }
    fn user(ra_username: Value, results: Value) -> Me {
        me(json!({"id": 1, "username": "u", "ra_username": ra_username, "ra_progression": {"total": 1, "results": results}}))
    }

    #[test]
    fn earned_matches_on_badge_name_not_achievement_id() {
        // The regression this module exists to prevent: ids differ from badge names, so matching
        // `earned.id` against `ra_id` would report nothing unlocked.
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(9001, "111", 1, 5), ach(9002, "222", 2, 10)]}}));
        let m = user(json!("pj"), json!([{"rom_ra_id": 10, "earned_achievements": [{"id": "222", "date": "2026-01-02 03:04:05"}]}]));
        let v = build(&r, &m);
        assert_eq!(v.state, RaState::Ready);
        assert_eq!((v.total, v.earned), (2, 1));
        assert!(!v.achievements[0].earned);
        assert!(v.achievements[1].earned);
        assert_eq!(v.achievements[1].earned_at.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!((v.points_total, v.points_earned), (15, 10));
    }

    #[test]
    fn badge_image_follows_earned_state() {
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "a", 1, 1), ach(2, "b", 2, 1)]}}));
        let m = user(json!("pj"), json!([{"rom_ra_id": 10, "earned_achievements": [{"id": "a", "date": "2026-01-01"}]}]));
        let v = build(&r, &m);
        assert_eq!(v.achievements[0].badge.as_deref(), Some("/assets/romm/resources/b/a.png"));
        assert_eq!(v.achievements[1].badge.as_deref(), Some("/assets/romm/resources/b/b_lock.png"));
    }

    #[test]
    fn shared_badge_counts_are_right_even_if_attribution_is_a_guess() {
        // Three achievements share badge "x"; the user earned two of them.
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "x", 1, 1), ach(2, "x", 2, 1), ach(3, "x", 3, 1), ach(4, "y", 4, 1)]}}));
        let m = user(json!("pj"), json!([{"rom_ra_id": 10, "earned_achievements": [
            {"id": "x", "date": "2026-01-01"}, {"id": "x", "date": "2026-01-02"}, {"id": "y", "date": "2026-01-03"}]}]));
        let v = build(&r, &m);
        assert_eq!(v.earned, 3);
        let flags: Vec<bool> = v.achievements.iter().map(|a| a.earned).collect();
        assert_eq!(flags, [true, true, false, true]);
        // Each earned entry is used once, in order.
        assert_eq!(v.achievements[0].earned_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(v.achievements[1].earned_at.as_deref(), Some("2026-01-02T00:00:00Z"));
    }

    #[test]
    fn progress_for_a_different_game_is_ignored() {
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "a", 1, 1)]}}));
        let m = user(json!("pj"), json!([{"rom_ra_id": 99, "earned_achievements": [{"id": "a", "date": "2026-01-01"}]}]));
        let v = build(&r, &m);
        assert_eq!(v.earned, 0);
        assert!(!v.progress_missing, "the user has progression, just not for this game");
    }

    #[test]
    fn never_refreshed_is_distinguished_from_not_played() {
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "a", 1, 1)]}}));
        let m = me(json!({"id": 1, "username": "u", "ra_username": "pj", "ra_progression": null}));
        let v = build(&r, &m);
        assert!(v.progress_missing);
        assert_eq!(v.state, RaState::Ready);
    }

    #[test]
    fn unlinked_user_still_sees_the_set() {
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "a", 1, 3)]}}));
        for name in [json!(null), json!(""), json!("   ")] {
            let m = me(json!({"id": 1, "username": "u", "ra_username": name, "ra_progression": null}));
            let v = build(&r, &m);
            assert_eq!(v.state, RaState::NotLinked);
            assert_eq!(v.ra_username, None);
            assert_eq!((v.total, v.earned, v.points_total), (1, 0, 3));
        }
    }

    #[test]
    fn game_without_a_set_reports_no_set() {
        let m = user(json!("pj"), json!([]));
        assert_eq!(build(&rom(json!({})), &m).state, RaState::NoSet);
        assert_eq!(build(&rom(json!({"ra_id": 5, "merged_ra_metadata": null})), &m).state, RaState::NoSet);
        assert_eq!(build(&rom(json!({"ra_id": 5, "merged_ra_metadata": {"achievements": []}})), &m).state, RaState::NoSet);
        // `NoSet` wins over `NotLinked`: with no set there is nothing to link an account for.
        let unlinked = me(json!({"id": 1, "ra_username": null}));
        assert_eq!(build(&rom(json!({})), &unlinked).state, RaState::NoSet);
    }

    #[test]
    fn hardcore_is_flagged_and_its_date_preferred() {
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "a", 1, 1), ach(2, "b", 2, 1)]}}));
        let m = user(json!("pj"), json!([{"rom_ra_id": 10, "highest_award_kind": "beaten-hardcore", "earned_achievements": [
            {"id": "a", "date": "2026-01-01 10:00:00", "date_hardcore": "2026-01-01 10:00:05"},
            {"id": "b", "date": "2026-02-01 10:00:00"}]}]));
        let v = build(&r, &m);
        assert!(v.achievements[0].hardcore && !v.achievements[1].hardcore);
        assert_eq!(v.achievements[0].earned_at.as_deref(), Some("2026-01-01T10:00:05Z"));
        assert_eq!((v.earned, v.earned_hardcore), (2, 1));
        assert_eq!(v.last_earned.as_deref(), Some("2026-02-01T10:00:00Z"));
        assert_eq!(v.award.as_deref(), Some("beaten-hardcore"));
    }

    #[test]
    fn catalog_is_sorted_by_display_order_with_missing_last() {
        let mut no_order = ach(5, "e", 0, 1);
        no_order.as_object_mut().unwrap().remove("display_order");
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [no_order, ach(2, "b", 20, 1), ach(1, "a", 10, 1)]}}));
        let v = build(&r, &user(json!("pj"), json!([])));
        let ids: Vec<i64> = v.achievements.iter().map(|a| a.id).collect();
        assert_eq!(ids, [1, 2, 5]);
    }

    #[test]
    fn earned_id_may_arrive_as_a_number() {
        let e: RaEarned = serde_json::from_value(json!({"id": 250336, "date": "2026-01-01"})).unwrap();
        assert_eq!(e.id, "250336");
        assert!(serde_json::from_value::<RaEarned>(json!({"id": null})).is_err());
    }

    #[test]
    fn tolerates_missing_and_null_fields() {
        let r = rom(json!({"ra_id": 1, "merged_ra_metadata": {"achievements": [{"ra_id": null, "title": null, "points": null, "badge_id": null, "extra": 1}]}}));
        let v = build(&r, &user(json!("pj"), json!([{"rom_ra_id": 1, "earned_achievements": [{"id": "anything"}]}])));
        assert_eq!(v.total, 1);
        assert_eq!(v.earned, 0, "an achievement with no badge name can never match");
        assert_eq!(v.achievements[0].title, "");
    }

    #[test]
    fn normalizes_the_timestamp_shapes_retroachievements_uses() {
        assert_eq!(normalize_ts("2026-01-02 03:04:05"), "2026-01-02T03:04:05Z");
        assert_eq!(normalize_ts("2026-01-02T03:04:05"), "2026-01-02T03:04:05Z");
        assert_eq!(normalize_ts("2026-01-02T03:04:05Z"), "2026-01-02T03:04:05Z");
        assert_eq!(normalize_ts("2026-01-02T05:04:05+02:00"), "2026-01-02T03:04:05Z");
        assert_eq!(normalize_ts("2026-01-02"), "2026-01-02T00:00:00Z");
        assert_eq!(normalize_ts("2026-01-02 03:04:05.250"), "2026-01-02T03:04:05Z");
        assert_eq!(normalize_ts("  garbage "), "garbage");
    }

    #[test]
    fn view_survives_a_json_round_trip_for_the_offline_cache() {
        let r = rom(json!({"ra_id": 10, "merged_ra_metadata": {"achievements": [ach(1, "a", 1, 1)]}}));
        let v = build(&r, &user(json!("pj"), json!([{"rom_ra_id": 10, "earned_achievements": [{"id": "a", "date": "2026-01-01"}]}])));
        let back: AchievementsView = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back, v);
    }
}
