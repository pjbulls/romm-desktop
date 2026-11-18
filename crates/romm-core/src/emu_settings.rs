//! Settings the app takes over for the user's own emulators.
//!
//! The emulators stay the user's: installed by them, found by `detect`, run as before. What this
//! adds is a way to say "play PS2 games at 3x" once, in this app, and have it true every time a
//! game is launched from here, without opening the emulator's own settings.
//!
//! What can be controlled is declared per emulator in its profile (`[[settings]]`), each choice
//! carrying the effects it has (see `profiles::Apply`): command-line arguments, keys in an INI or
//! TOML settings file, or libretro core options in RetroArch's per-core options file.
//!
//! Rules this module keeps, because it edits files that belong to someone else:
//!
//! * Nothing is touched until the user picks a value. "Don't change" is the default.
//! * Edits are minimal: the one key is changed, and comments, ordering and spacing around it stay.
//! * A settings file that doesn't exist is not created (RetroArch's per-core options excepted, see
//!   below). An emulator that has never run has no settings file, and one we wrote would stop it
//!   running its first-time setup. The user gets a message instead.
//! * The original of a file is copied aside before it is first changed.
//! * Nothing here can stop a game starting: problems become messages.
//!
//! Planning (`plan`) only works out what to do; `Tuning::apply` does the writing, just before launch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::profiles::{self, Apply, Profile, Root, Setting, TomlKind};
use crate::retroarch::{self, Layout};

/// What the user chose: scope -> setting id -> choice value. A scope is an emulator's profile id,
/// or `retroarch:<core>` for one libretro core (each names its options differently). A missing
/// entry, or an empty value, means "don't change".
pub type Values = BTreeMap<String, BTreeMap<String, String>>;

pub fn scope_id(profile_id: &str, core: Option<&str>) -> String {
    match core {
        Some(c) => format!("{profile_id}:{c}"),
        None => profile_id.to_string(),
    }
}

// ------------------------------------------------------------------------------------ schema

/// An emulator (or one RetroArch core) and the settings the app can control for it.
#[derive(Debug, Clone)]
pub struct Scope {
    pub scope: String,
    pub profile_id: String,
    pub name: String,
    pub core: Option<String>,
    pub settings: Vec<Setting>,
}

impl Scope {
    /// Does any choice here edit a file of the emulator's? (Otherwise everything is passed on the
    /// command line, and the emulator's own files are never touched.)
    pub fn changes_files(&self) -> bool {
        self.settings.iter().any(|s| {
            s.apply.iter().chain(s.choices.iter().flat_map(|c| c.apply.iter())).any(|a| !matches!(a, Apply::Args { .. }))
        })
    }
}

/// Every scope that has settings, in profile order (RetroArch's cores in the order declared).
pub fn scopes() -> Vec<Scope> {
    let mut out: Vec<Scope> = Vec::new();
    for p in profiles::builtin() {
        for s in &p.settings {
            let scope = scope_id(&p.id, s.core.as_deref());
            match out.iter_mut().find(|o| o.scope == scope) {
                Some(o) => o.settings.push(s.clone()),
                None => {
                    let name = match &s.core {
                        Some(c) => format!("{} · {}", p.name, retroarch::core_label(c)),
                        None => p.name.clone(),
                    };
                    out.push(Scope { scope, profile_id: p.id.clone(), name, core: s.core.clone(), settings: vec![s.clone()] });
                }
            }
        }
    }
    out
}

/// Keeps only what the schema knows: an emulator or setting a newer version dropped, or a value
/// that is no longer one of its choices, is forgotten rather than carried around forever. Empty
/// values ("don't change") are not stored.
pub fn sanitize(values: Values) -> Values {
    let known = scopes();
    let mut out = Values::new();
    for (scope, chosen) in values {
        let Some(sc) = known.iter().find(|k| k.scope == scope) else { continue };
        let kept: BTreeMap<String, String> = chosen
            .into_iter()
            .filter(|(id, v)| !v.is_empty() && sc.settings.iter().any(|s| &s.id == id && s.choices.iter().any(|c| &c.value == v)))
            .collect();
        if !kept.is_empty() {
            out.insert(scope, kept);
        }
    }
    out
}

// ------------------------------------------------------------------------------------ planning

/// Everything planning needs to know about one emulator on this machine.
pub struct PlanCtx<'a> {
    pub data_root: &'a Path,
    pub config_root: &'a Path,
    /// RetroArch's layout, when the emulator is RetroArch.
    pub retroarch: Option<&'a Layout>,
    /// The libretro core this game will run in, when the emulator is RetroArch.
    pub core: Option<&'a str>,
}

/// A change to one settings file, with every path already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// `section` is empty for a file without sections (RetroArch's `.opt`), where `quote` writes
    /// values as `key = "value"` the way RetroArch does.
    Ini { section: String, sets: Vec<(String, String)>, quote: bool },
    Toml { path: Vec<String>, value: String, kind: TomlKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    /// Shown in messages: "PCSX2 · Internal resolution".
    pub label: String,
    pub scope: String,
    pub path: PathBuf,
    pub op: Op,
    /// May the file be created? Only RetroArch's per-core options: RetroArch makes that file
    /// itself on first use, so making it is not a departure from what it would do.
    pub create: bool,
    /// When the file is created: start from this one's contents (see `retroarch::OptionsTarget`).
    pub seed: Option<PathBuf>,
}

/// What a game's launch should do for the chosen settings.
#[derive(Debug, Default, Clone)]
pub struct Tuning {
    /// Added to the command line ahead of the user's own extra args (so those still win).
    pub args: Vec<String>,
    pub edits: Vec<FileEdit>,
    /// Things planning found that the user should hear about.
    pub notes: Vec<String>,
}

fn subst(s: &str, value: &str) -> String {
    s.replace("{value}", value)
}

/// Works out the launch arguments and file edits for `profile` from `values`. Does no writing.
pub fn plan(profile: &Profile, values: &Values, cx: &PlanCtx) -> Tuning {
    let mut t = Tuning::default();
    for setting in &profile.settings {
        // A RetroArch setting belongs to one core, and only applies when that core is the one running.
        if let Some(core) = &setting.core {
            if cx.core != Some(core.as_str()) {
                continue;
            }
        }
        let scope = scope_id(&profile.id, setting.core.as_deref());
        let Some(chosen) = values.get(&scope).and_then(|m| m.get(&setting.id)).filter(|v| !v.is_empty()) else { continue };
        // A value the profile no longer offers is ignored, not guessed at.
        let Some(choice) = setting.choices.iter().find(|c| &c.value == chosen) else { continue };

        let label = match &setting.core {
            Some(c) => format!("{} ({}) · {}", profile.name, retroarch::core_label(c), setting.label),
            None => format!("{} · {}", profile.name, setting.label),
        };
        let effects = if choice.apply.is_empty() { &setting.apply } else { &choice.apply };
        for effect in effects {
            match effect {
                Apply::Args { args } => t.args.extend(args.iter().map(|a| subst(a, &choice.value))),
                Apply::Ini { root, file, section, sets } => t.edits.push(FileEdit {
                    label: label.clone(),
                    scope: scope.clone(),
                    path: root_dir(*root, cx).join(file),
                    op: Op::Ini {
                        section: section.clone(),
                        sets: sets.iter().map(|kv| (kv.key.clone(), subst(&kv.value, &choice.value))).collect(),
                        quote: false,
                    },
                    create: false,
                    seed: None,
                }),
                Apply::Toml { root, file, path, value, kind } => t.edits.push(FileEdit {
                    label: label.clone(),
                    scope: scope.clone(),
                    path: root_dir(*root, cx).join(file),
                    op: Op::Toml { path: path.split('.').map(str::to_string).collect(), value: subst(value, &choice.value), kind: *kind },
                    create: false,
                    seed: None,
                }),
                Apply::RetroarchOption { sets } => {
                    let (Some(layout), Some(core)) = (cx.retroarch, cx.core) else { continue };
                    if !layout.root.is_dir() {
                        t.notes.push(format!("{label}: RetroArch hasn't created its folder yet. Start RetroArch once, then launch again."));
                        continue;
                    }
                    let target = retroarch::options_target(layout, core);
                    let n = retroarch::overriding_option_files(layout, core);
                    if n > 0 {
                        t.notes.push(format!(
                            "{label}: RetroArch has {n} per-game or per-folder option file{} for this core, and those take priority over this setting.",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    t.edits.push(FileEdit {
                        label: label.clone(),
                        scope: scope.clone(),
                        path: target.path,
                        op: Op::Ini {
                            section: String::new(),
                            sets: sets.iter().map(|kv| (kv.key.clone(), subst(&kv.value, &choice.value))).collect(),
                            quote: true,
                        },
                        create: true,
                        seed: target.seed,
                    });
                }
            }
        }
    }
    t
}

fn root_dir<'a>(root: Root, cx: &'a PlanCtx) -> &'a Path {
    match root {
        Root::Data => cx.data_root,
        Root::Config => cx.config_root,
    }
}

// ------------------------------------------------------------------------------------ applying

impl Tuning {
    pub fn is_empty(&self) -> bool {
        self.args.is_empty() && self.edits.is_empty()
    }

    /// Writes the planned file edits, copying each original into `backup_dir` first. Returns what
    /// the user should be told. Never fails: a setting that can't be applied is a message, and the
    /// game still starts.
    pub fn apply(&self, backup_dir: &Path) -> Vec<String> {
        let mut notes = self.notes.clone();
        for edit in &self.edits {
            if let Err(msg) = apply_edit(edit, backup_dir) {
                notes.push(msg);
            }
        }
        notes
    }
}

fn apply_edit(e: &FileEdit, backup_dir: &Path) -> Result<(), String> {
    let existing = match std::fs::read(&e.path) {
        Ok(bytes) => Some(
            String::from_utf8(bytes)
                .map_err(|_| format!("{}: {} isn't UTF-8 text, so it was left alone.", e.label, e.path.display()))?,
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{}: couldn't read {}: {err}", e.label, e.path.display())),
    };

    let base = match (&existing, e.create) {
        (Some(text), _) => text.clone(),
        (None, true) => e.seed.as_ref().and_then(|s| std::fs::read_to_string(s).ok()).unwrap_or_default(),
        (None, false) => {
            return Err(format!(
                "{}: {} doesn't exist yet. Start the emulator once so it creates it, then launch again.",
                e.label,
                e.path.display()
            ))
        }
    };

    let patched = match &e.op {
        Op::Ini { section, sets, quote } => patch_ini(&base, section, sets, *quote),
        Op::Toml { path, value, kind } => {
            patch_toml(&base, path, value, *kind).map_err(|m| format!("{}: {} {m}", e.label, e.path.display()))?
        }
    };
    if existing.as_deref() == Some(patched.as_str()) {
        return Ok(()); // already as wanted: don't touch the file
    }

    if existing.is_some() {
        backup_once(&e.path, &e.scope, backup_dir)
            .map_err(|err| format!("{}: couldn't back up {} first ({err}), so it was left alone.", e.label, e.path.display()))?;
    }
    crate::saves::write_atomic(&e.path, patched.as_bytes())
        .map_err(|err| format!("{}: couldn't write {}: {err}", e.label, e.path.display()))
}

/// Keeps the first version of a file this app changed, so the user can always get back to what
/// the emulator itself wrote. Later changes don't replace it.
fn backup_once(path: &Path, scope: &str, backup_dir: &Path) -> std::io::Result<()> {
    let scope_dir: String = scope.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "settings".into());
    let dest = backup_dir.join(scope_dir).join(format!("{name}.orig"));
    if dest.exists() {
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::copy(path, dest).map(|_| ())
}

// ------------------------------------------------------------------------------------ INI

enum Line<'a> {
    Section(&'a str),
    Key(&'a str),
    Other,
}

fn classify(line: &str) -> Line<'_> {
    let t = line.trim();
    if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
        return Line::Other;
    }
    if t.starts_with('[') {
        return match t.find(']') {
            Some(end) => Line::Section(&t[1..end]),
            None => Line::Other,
        };
    }
    match line.find('=') {
        Some(i) => Line::Key(line[..i].trim()),
        None => Line::Other,
    }
}

fn split_terminator(line: &str) -> (&str, &str) {
    if let Some(b) = line.strip_suffix("\r\n") {
        (b, "\r\n")
    } else if let Some(b) = line.strip_suffix('\n') {
        (b, "\n")
    } else {
        (line, "")
    }
}

/// Sets `sets` in `section` of an INI file's text and returns the new text, changing as little as
/// possible: an existing key keeps its position, spacing and line ending, a missing one is added
/// at the end of its section, a missing section at the end of the file. Comments are never
/// treated as keys. `section` is empty for keys outside any section (before the first `[...]`).
/// Keys are matched exactly (the Qt-style `key\default` is a different key from `key`).
pub fn patch_ini(text: &str, section: &str, sets: &[(String, String)], quote: bool) -> String {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };
    let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = body.split_inclusive('\n').map(str::to_string).collect();
    for (key, value) in sets {
        let rendered = if quote { format!("\"{value}\"") } else { value.clone() };
        set_key(&mut lines, section, key, &rendered, eol);
    }
    format!("{bom}{}", lines.concat())
}

fn set_key(lines: &mut Vec<String>, section: &str, key: &str, value: &str, eol: &str) {
    let mut current = String::new();
    let mut section_exists = section.is_empty();
    let mut found = false;
    let mut last_content: Option<usize> = None;
    let mut spaced: Option<bool> = None;

    for (i, line) in lines.iter_mut().enumerate() {
        match classify(line) {
            Line::Section(name) => {
                current = name.to_string();
                if current == section {
                    section_exists = true;
                    last_content = Some(i);
                }
            }
            Line::Key(k) if current == section => {
                last_content = Some(i);
                let (body, term) = split_terminator(line);
                let eq = body.find('=').expect("a key line has an '='");
                if spaced.is_none() {
                    spaced = Some(body[..eq].ends_with(' '));
                }
                if k == key {
                    let after = &body[eq + 1..];
                    let lead = after.len() - after.trim_start().len();
                    let replaced = format!("{}{}{}{}", &body[..=eq], &after[..lead], value, term);
                    *line = replaced;
                    found = true;
                }
            }
            _ => {}
        }
    }
    if found {
        return;
    }

    let line_for = |spaced: bool| format!("{key}{}{value}{eol}", if spaced { " = " } else { "=" });
    if section_exists {
        // Straight after the section's last key (or its header), or the top of the file for keys
        // outside any section when there are none yet.
        let at = last_content.map(|i| i + 1).unwrap_or(0);
        if at > 0 && !lines[at - 1].ends_with('\n') {
            lines[at - 1].push_str(eol);
        }
        lines.insert(at, line_for(spaced.unwrap_or(true)));
    } else {
        if let Some(last) = lines.last_mut() {
            if !last.ends_with('\n') {
                last.push_str(eol);
            }
        }
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(eol.to_string());
        }
        lines.push(format!("[{section}]{eol}"));
        lines.push(line_for(true));
    }
}

// ------------------------------------------------------------------------------------ TOML

/// Sets the value at a dotted `path` in a TOML file's text, keeping everything else (comments,
/// order, formatting) and the comment on the line it replaces. Missing tables on the way are
/// created. `Err` is a sentence ending the message "<file> ...", and means the text was not changed.
pub fn patch_toml(text: &str, path: &[String], value: &str, kind: TomlKind) -> Result<String, String> {
    use toml_edit::{DocumentMut, Item, Table, Value};

    let mut doc: DocumentMut = text.parse().map_err(|e| format!("isn't valid TOML ({e}), so it was left alone."))?;
    let (key, parents) = path.split_last().ok_or_else(|| "has no setting path.".to_string())?;

    let mut table: &mut Table = doc.as_table_mut();
    for seg in parents {
        let item = table.entry(seg).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        table = item.as_table_mut().ok_or_else(|| format!("has `{seg}` where a table was expected, so it was left alone."))?;
    }

    let new: Value = match kind {
        TomlKind::Int => value.parse::<i64>().map_err(|_| format!("can't take `{value}` as a whole number."))?.into(),
        TomlKind::Bool => match value {
            "true" => true.into(),
            "false" => false.into(),
            _ => return Err(format!("can't take `{value}` as true or false.")),
        },
        TomlKind::String => value.into(),
    };
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            let mut replacement = new;
            *replacement.decor_mut() = decor;
            *old = replacement;
        }
        _ => {
            table.insert(key, Item::Value(new));
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests;
