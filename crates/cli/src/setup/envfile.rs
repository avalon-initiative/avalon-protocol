//! The node's environment file: the same `KEY=value` format `systemd`'s `EnvironmentFile=` reads,
//! merged idempotently so that re-running setup never clobbers what an operator already has.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Raw(String),
    /// `raw` is the original text, kept byte-for-byte until the value is changed.
    Kv {
        key: String,
        value: String,
        raw: Option<String>,
    },
}

/// An environment file that keeps comments, blank lines and unmanaged lines exactly as written.
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
                if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                    return Line::Raw(line.to_string());
                }
                match trimmed.split_once('=') {
                    Some((k, v)) if !k.trim().is_empty() => Line::Kv {
                        key: k.trim().to_string(),
                        value: unquote(v.trim()),
                        raw: Some(line.to_string()),
                    },
                    _ => Line::Raw(line.to_string()),
                }
            })
            .collect();
        Self { lines }
    }

    /// The effective value: the last assignment wins, as it does when systemd reads the file.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().rev().find_map(|l| match l {
            Line::Kv { key: k, value, .. } if k == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Sets the effective (last) assignment, appending one when the key is new.
    pub fn set(&mut self, key: &str, value: &str) {
        for l in self.lines.iter_mut().rev() {
            if let Line::Kv {
                key: k,
                value: v,
                raw,
            } = l
            {
                if k == key {
                    *v = value.to_string();
                    *raw = None;
                    return;
                }
            }
        }
        self.lines.push(Line::Kv {
            key: key.to_string(),
            value: value.to_string(),
            raw: None,
        });
    }

    /// Removes every assignment of `key`.
    pub fn remove(&mut self, key: &str) {
        self.lines
            .retain(|l| !matches!(l, Line::Kv { key: k, .. } if k == key));
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// All key/value pairs, later duplicates winning, as a process environment.
    pub fn to_map(&self) -> BTreeMap<String, String> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Line::Kv { key, value, .. } => Some((key.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn render(&self) -> Result<String, String> {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Raw(s) => out.push_str(s),
                Line::Kv { raw: Some(raw), .. } => out.push_str(raw),
                Line::Kv {
                    key,
                    value,
                    raw: None,
                } => {
                    out.push_str(key);
                    out.push('=');
                    out.push_str(&quote(key, value)?);
                }
            }
            out.push('\n');
        }
        Ok(out)
    }
}

fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].to_string();
    }
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let mut out = String::new();
        let mut chars = v[1..v.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    v.to_string()
}

/// Bare when every character is safe for both a shell and systemd; single-quoted when that is
/// enough; otherwise double-quoted with `\`, `"`, `$` and backtick escaped. systemd's
/// `EnvironmentFile=` reads a backslash before any character as that character and does no `$`
/// or `%` expansion, and a shell sourcing the file sees the same value.
fn quote(key: &str, v: &str) -> Result<String, String> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./:@%+,=-".contains(c);
    if !v.is_empty() && v.chars().all(safe) {
        return Ok(v.to_string());
    }
    if v.contains('\n') || v.contains('\r') {
        return Err(format!("the value of {key} contains a line break"));
    }
    if !v.contains('\'') && !v.contains('\\') {
        return Ok(format!("'{v}'"));
    }
    let mut out = String::from("\"");
    for c in v.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
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
/// An existing value that differs is only replaced (or removed) when `may_replace(key)` allows it.
pub fn plan(
    existing: &EnvFile,
    desired: &[(&'static str, Option<String>)],
    may_replace: &dyn Fn(&str) -> bool,
) -> (EnvFile, Vec<PlanItem>) {
    let mut out = existing.clone();
    let mut items = Vec::new();
    for (key, want) in desired {
        let replace = may_replace(key);
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

/// Hides secrets in a value for display: the password of `scheme://user:password@host/...` and
/// any `password`/`passwd`/`pwd`/`sslpassword` query or key/value parameter. Anything under a
/// `DATABASE_URL` that cannot be parsed is hidden entirely.
pub fn redact(key: &str, value: &str) -> String {
    if key != "DATABASE_URL" {
        return value.to_string();
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return "<redacted>".to_string();
    };
    let (authority_path, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let authority_end = authority_path.find('/').unwrap_or(authority_path.len());
    let (authority, path) = authority_path.split_at(authority_end);
    let authority = match authority.rsplit_once('@') {
        Some((cred, host)) => match cred.split_once(':') {
            Some((user, _)) => format!("{user}:***@{host}"),
            None => authority.to_string(),
        },
        None => authority.to_string(),
    };
    let mut out = format!("{scheme}://{authority}{path}");
    if let Some(q) = query {
        let params: Vec<String> = q
            .split('&')
            .map(|p| match p.split_once('=') {
                Some((k, _))
                    if k.to_ascii_lowercase().contains("pass") || k.eq_ignore_ascii_case("pwd") =>
                {
                    format!("{k}=***")
                }
                _ => p.to_string(),
            })
            .collect();
        out.push('?');
        out.push_str(&params.join("&"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(k: &'static str, v: Option<&str>) -> (&'static str, Option<String>) {
        (k, v.map(str::to_string))
    }

    fn no(_: &str) -> bool {
        false
    }

    #[test]
    fn parse_render_round_trip_keeps_comments_and_unknown_keys() {
        let text = "# mine\nFOO=bar\n\nAVALON_NETWORK_ID=x\n";
        let f = EnvFile::parse(text);
        assert_eq!(f.render().unwrap(), text);
        assert_eq!(f.get("FOO"), Some("bar"));
    }

    #[test]
    fn unmanaged_lines_are_preserved_byte_for_byte() {
        let text = "FOO=\"it's\"   \nBAR = 'x y'\n; note\nBAZ=a#b\n";
        let mut f = EnvFile::parse(text);
        f.set("NEW", "1");
        assert_eq!(f.render().unwrap(), format!("{text}NEW=1\n"));
    }

    #[test]
    fn values_with_special_characters_round_trip() {
        let values = [
            "postgres://u:p%40ss@h:5432/db?sslmode=require&x=1",
            "postgres://u:p'w@h/db",
            "it's",
            "a#b",
            "k=v",
            "two words",
            "cost $5 and `tick`",
            "back\\slash",
            "mixed ' \" \\ $ `",
            "",
        ];
        for v in values {
            let mut f = EnvFile::default();
            f.set("K", v);
            let text = f.render().unwrap();
            assert_eq!(
                EnvFile::parse(&text).get("K"),
                Some(v),
                "{v:?} via {text:?}"
            );
        }
    }

    #[test]
    fn quoting_choices_are_systemd_and_shell_safe() {
        let render = |v: &str| {
            let mut f = EnvFile::default();
            f.set("K", v);
            f.render().unwrap()
        };
        assert_eq!(render("127.0.0.1:8080"), "K=127.0.0.1:8080\n");
        assert_eq!(render("a b"), "K='a b'\n");
        assert_eq!(render("p'w"), "K=\"p'w\"\n");
        assert_eq!(render("a$b'"), "K=\"a\\$b'\"\n");
    }

    #[test]
    fn a_quote_in_a_value_no_longer_breaks_rerender_and_errors_never_show_values() {
        let f = EnvFile::parse("FOO=\"it's\"\n");
        assert_eq!(f.render().unwrap(), "FOO=\"it's\"\n");
        let mut g = EnvFile::default();
        g.set("DATABASE_URL", "postgres://u:s3cret\nx@h/db");
        let err = g.render().unwrap_err();
        assert!(!err.contains("s3cret"), "{err}");
    }

    #[test]
    fn duplicate_keys_get_and_set_agree() {
        let mut f = EnvFile::parse("A=1\nA=2\n");
        assert_eq!(f.get("A"), Some("2"));
        f.set("A", "3");
        assert_eq!(f.get("A"), Some("3"));
        assert_eq!(f.render().unwrap(), "A=1\nA=3\n");
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
        let (out, items) = plan(&existing, &want, &no);
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
    fn plan_replaces_and_removes_only_where_allowed() {
        let existing = EnvFile::parse("B=2\nDATABASE_URL=x\nP=old\n");
        let want = [
            d("B", Some("9")),
            d("DATABASE_URL", None),
            d("P", Some("new")),
        ];
        let (out, items) = plan(&existing, &want, &|k| k != "P");
        assert_eq!(out.get("B"), Some("9"));
        assert_eq!(out.get("DATABASE_URL"), None);
        assert_eq!(out.get("P"), Some("old"));
        assert_eq!(items[0].action, Action::Replaced);
        assert_eq!(items[1].action, Action::Removed);
        assert_eq!(items[2].action, Action::KeptDifferent);
    }

    #[test]
    fn second_plan_over_its_own_output_changes_nothing() {
        let want = [d("A", Some("1")), d("B", Some("two words"))];
        let (first, _) = plan(&EnvFile::default(), &want, &no);
        let reparsed = EnvFile::parse(&first.render().unwrap());
        let (second, items) = plan(&reparsed, &want, &no);
        assert_eq!(second.render().unwrap(), first.render().unwrap());
        assert!(items.iter().all(|i| i.action == Action::Kept));
    }

    #[test]
    fn redact_hides_the_password_and_password_parameters() {
        assert_eq!(
            redact("DATABASE_URL", "postgres://avalon:secret@db:5432/avalon"),
            "postgres://avalon:***@db:5432/avalon"
        );
        assert_eq!(
            redact(
                "DATABASE_URL",
                "postgres://avalon@db/avalon?password=hunter2&sslmode=require"
            ),
            "postgres://avalon@db/avalon?password=***&sslmode=require"
        );
        assert_eq!(
            redact("DATABASE_URL", "postgres://u:p'w@db/x?sslpassword=zz"),
            "postgres://u:***@db/x?sslpassword=***"
        );
        assert_eq!(redact("DATABASE_URL", "not a url"), "<redacted>");
        assert_eq!(redact("AVALON_NETWORK_ID", "n"), "n");
    }
}
