//! The node's environment file: the same `KEY=value` format `systemd`'s `EnvironmentFile=` reads,
//! merged idempotently so that re-running setup never clobbers what an operator already has.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Raw(String),
    Kv(String, String),
}

/// An environment file that keeps comments, blank lines and unmanaged keys in place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvFile {
    lines: Vec<Line>,
}

impl EnvFile {
    pub fn parse(text: &str) -> Self {
        let lines = text
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return Line::Raw(line.to_string());
                }
                match trimmed.split_once('=') {
                    Some((k, v)) if !k.trim().is_empty() => {
                        Line::Kv(k.trim().to_string(), unquote(v.trim()))
                    }
                    _ => Line::Raw(line.to_string()),
                }
            })
            .collect();
        Self { lines }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().rev().find_map(|l| match l {
            Line::Kv(k, v) if k == key => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn set(&mut self, key: &str, value: &str) {
        for l in self.lines.iter_mut() {
            if let Line::Kv(k, v) = l {
                if k == key {
                    *v = value.to_string();
                    return;
                }
            }
        }
        self.lines
            .push(Line::Kv(key.to_string(), value.to_string()));
    }

    pub fn remove(&mut self, key: &str) {
        self.lines
            .retain(|l| !matches!(l, Line::Kv(k, _) if k == key));
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// All key/value pairs, later duplicates winning, as a process environment.
    pub fn to_map(&self) -> BTreeMap<String, String> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Line::Kv(k, v) => Some((k.clone(), v.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn render(&self) -> Result<String, String> {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Raw(s) => out.push_str(s),
                Line::Kv(k, v) => {
                    out.push_str(k);
                    out.push('=');
                    out.push_str(&quote(v)?);
                }
            }
            out.push('\n');
        }
        Ok(out)
    }
}

fn unquote(v: &str) -> String {
    for q in ['\'', '"'] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// Bare when every character is shell- and systemd-safe, single-quoted otherwise.
fn quote(v: &str) -> Result<String, String> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./:@%+,=-".contains(c);
    if !v.is_empty() && v.chars().all(safe) {
        return Ok(v.to_string());
    }
    if v.contains('\'') || v.contains('\n') {
        return Err(format!("value {v:?} contains a quote or newline"));
    }
    Ok(format!("'{v}'"))
}

/// What happened to one managed key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Added,
    Kept,
    Replaced,
    Removed,
    /// The requested value differs from the existing one and replacing was not allowed.
    KeptDifferent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub key: &'static str,
    pub action: Action,
    /// The value now in the file (`None` when absent).
    pub value: Option<String>,
    /// The value that was asked for when it was not applied.
    pub requested: Option<String>,
}

/// Merges `desired` into `existing`. A `None` desired value means the key should be absent.
/// Existing values that differ are only replaced (or removed) when `replace` is set.
pub fn plan(
    existing: &EnvFile,
    desired: &[(&'static str, Option<String>)],
    replace: bool,
) -> (EnvFile, Vec<PlanItem>) {
    let mut out = existing.clone();
    let mut items = Vec::new();
    for (key, want) in desired {
        let have = existing.get(key).map(str::to_string);
        let item = match (have, want) {
            (None, None) => continue,
            (None, Some(w)) => {
                out.set(key, w);
                PlanItem {
                    key,
                    action: Action::Added,
                    value: Some(w.clone()),
                    requested: None,
                }
            }
            (Some(h), Some(w)) if &h == w => PlanItem {
                key,
                action: Action::Kept,
                value: Some(h),
                requested: None,
            },
            (Some(h), Some(w)) => {
                if replace {
                    out.set(key, w);
                    PlanItem {
                        key,
                        action: Action::Replaced,
                        value: Some(w.clone()),
                        requested: None,
                    }
                } else {
                    PlanItem {
                        key,
                        action: Action::KeptDifferent,
                        value: Some(h),
                        requested: Some(w.clone()),
                    }
                }
            }
            (Some(h), None) => {
                if replace {
                    out.remove(key);
                    PlanItem {
                        key,
                        action: Action::Removed,
                        value: None,
                        requested: None,
                    }
                } else {
                    PlanItem {
                        key,
                        action: Action::KeptDifferent,
                        value: Some(h),
                        requested: None,
                    }
                }
            }
        };
        items.push(item);
    }
    (out, items)
}

/// Hides the password of a `scheme://user:password@host/...` URL.
pub fn redact(key: &str, value: &str) -> String {
    if key != "DATABASE_URL" {
        return value.to_string();
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return "<redacted>".to_string();
    };
    match rest.rsplit_once('@') {
        Some((cred, host)) => match cred.split_once(':') {
            Some((user, _)) => format!("{scheme}://{user}:***@{host}"),
            None => value.to_string(),
        },
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(k: &'static str, v: Option<&str>) -> (&'static str, Option<String>) {
        (k, v.map(str::to_string))
    }

    #[test]
    fn parse_render_round_trip_keeps_comments_and_unknown_keys() {
        let text = "# mine\nFOO=bar\n\nAVALON_NETWORK_ID=x\n";
        let f = EnvFile::parse(text);
        assert_eq!(f.render().unwrap(), text);
        assert_eq!(f.get("FOO"), Some("bar"));
    }

    #[test]
    fn values_with_special_characters_are_quoted_and_read_back() {
        let mut f = EnvFile::default();
        f.set(
            "DATABASE_URL",
            "postgres://u:p%40ss@h:5432/db?sslmode=require&x=1",
        );
        let text = f.render().unwrap();
        assert!(text.contains("DATABASE_URL='postgres://"));
        assert_eq!(
            EnvFile::parse(&text).get("DATABASE_URL"),
            Some("postgres://u:p%40ss@h:5432/db?sslmode=require&x=1")
        );
    }

    #[test]
    fn plain_values_stay_bare() {
        let mut f = EnvFile::default();
        f.set("AVALON_SERVER_ADDR", "127.0.0.1:8080");
        assert_eq!(f.render().unwrap(), "AVALON_SERVER_ADDR=127.0.0.1:8080\n");
    }

    #[test]
    fn single_quote_in_value_is_refused() {
        let mut f = EnvFile::default();
        f.set("K", "a'b");
        assert!(f.render().is_err());
    }

    #[test]
    fn plan_adds_keeps_and_never_clobbers_without_replace() {
        let existing = EnvFile::parse("A=1\nB=2\nEXTRA=keep\n");
        let want = [
            d("A", Some("1")),
            d("B", Some("9")),
            d("C", Some("3")),
            d("D", None),
        ];
        let (out, items) = plan(&existing, &want, false);
        assert_eq!(out.get("A"), Some("1"));
        assert_eq!(out.get("B"), Some("2"));
        assert_eq!(out.get("C"), Some("3"));
        assert_eq!(out.get("EXTRA"), Some("keep"));
        let actions: Vec<_> = items.iter().map(|i| i.action.clone()).collect();
        assert_eq!(
            actions,
            [Action::Kept, Action::KeptDifferent, Action::Added]
        );
        assert_eq!(items[1].requested.as_deref(), Some("9"));
    }

    #[test]
    fn plan_replaces_and_removes_with_replace() {
        let existing = EnvFile::parse("B=2\nDATABASE_URL=x\n");
        let want = [d("B", Some("9")), d("DATABASE_URL", None)];
        let (out, items) = plan(&existing, &want, true);
        assert_eq!(out.get("B"), Some("9"));
        assert_eq!(out.get("DATABASE_URL"), None);
        assert_eq!(items[0].action, Action::Replaced);
        assert_eq!(items[1].action, Action::Removed);
    }

    #[test]
    fn second_plan_over_its_own_output_changes_nothing() {
        let want = [d("A", Some("1")), d("B", Some("two words"))];
        let (first, _) = plan(&EnvFile::default(), &want, false);
        let reparsed = EnvFile::parse(&first.render().unwrap());
        let (second, items) = plan(&reparsed, &want, false);
        assert_eq!(second.render().unwrap(), first.render().unwrap());
        assert!(items.iter().all(|i| i.action == Action::Kept));
    }

    #[test]
    fn redact_hides_the_password_only() {
        assert_eq!(
            redact("DATABASE_URL", "postgres://avalon:secret@db:5432/avalon"),
            "postgres://avalon:***@db:5432/avalon"
        );
        assert_eq!(redact("AVALON_NETWORK_ID", "n"), "n");
    }
}
