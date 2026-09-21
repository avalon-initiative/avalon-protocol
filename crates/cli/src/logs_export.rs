//! `avalon logs export` — issue #659. Turns `avalon-server`'s own log file
//! (`make start`'s `_running/logs/avalon-server.log`, human-readable or
//! JSON-per-line depending on `AVALON_LOG_FORMAT`, see #265) into a
//! redacted, normalized, line-delimited-JSON export a hoster can safely
//! attach to a filed GitHub issue without hand-copying raw log text (which
//! may carry ANSI color codes, and worse, a real secret that ended up in a
//! log line).
//!
//! Redaction is two layers, deliberately separate (see [`redact_exact`] and
//! [`redact_shape`] doc comments for what each one is actually good for —
//! neither is a complete guarantee on its own).

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const USAGE: &str =
    "usage: avalon logs export [<file>] [--file <path>] [--tail <n>] [--since <rfc3339-timestamp>]";

/// Every env var this process might plausibly have logged the value of,
/// directly or indirectly (a connection string in a startup message, a key
/// echoed in a debug line, etc). Checked against the *exporting* shell's
/// own environment at export time, not against whatever environment
/// actually produced the log file — see [`redact_exact`].
pub const SECRET_ENV_VARS: &[&str] = &[
    "DATABASE_URL",
    "AVALON_SETTLEMENT_SIGNING_KEY",
    "AVALON_SETTLEMENT_SUBMIT_KEY",
    "AVALON_ADMIN_TOKEN",
    "AVALON_INTERNAL_ROLE_KEY",
    "AVALON_MANAGED_HOSTING_VERIFY_KEY",
];

const REDACTED: &str = "[REDACTED]";

pub struct ExportArgs {
    pub file: PathBuf,
    pub tail: Option<usize>,
    pub since: Option<OffsetDateTime>,
}

impl ExportArgs {
    /// `<file>` may be given positionally or via `--file`; `--file` wins if
    /// both are somehow given. Defaults to `make start`'s own
    /// `$(LOG_DIR)/avalon-server.log` (see the root `Makefile`'s `LOG_DIR`/
    /// `LOG_FILE` variables) so running this with no arguments does the
    /// obvious thing right after a `make start`.
    pub fn parse(raw_args: &[String]) -> Result<ExportArgs, String> {
        let mut file: Option<PathBuf> = None;
        let mut tail: Option<usize> = None;
        let mut since: Option<OffsetDateTime> = None;

        let mut i = 0;
        while i < raw_args.len() {
            match raw_args[i].as_str() {
                "--file" => {
                    let value = raw_args
                        .get(i + 1)
                        .ok_or_else(|| "--file requires a path".to_string())?;
                    file = Some(PathBuf::from(value));
                    i += 2;
                }
                "--tail" => {
                    let value = raw_args
                        .get(i + 1)
                        .ok_or_else(|| "--tail requires a number".to_string())?;
                    tail = Some(
                        value
                            .parse::<usize>()
                            .map_err(|_| format!("--tail: not a number: {value}"))?,
                    );
                    i += 2;
                }
                "--since" => {
                    let value = raw_args
                        .get(i + 1)
                        .ok_or_else(|| "--since requires an RFC 3339 timestamp".to_string())?;
                    since = Some(OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
                        format!("--since: not an RFC 3339 timestamp: {value}")
                    })?);
                    i += 2;
                }
                other if !other.starts_with("--") && file.is_none() => {
                    file = Some(PathBuf::from(other));
                    i += 1;
                }
                other => {
                    return Err(format!("unrecognized argument: {other}"));
                }
            }
        }

        let file = file.unwrap_or_else(default_log_file);
        Ok(ExportArgs { file, tail, since })
    }
}

/// `make start`'s `$(RUN_DIR)/logs/avalon-server.log`, i.e.
/// `_running/logs/avalon-server.log` relative to the repo root — matches
/// the root `Makefile`'s `RUN_DIR`/`LOG_DIR`/`LOG_FILE` variables exactly.
/// This command is expected to run from the repo root, same as every other
/// `make`/`avalon` command in this project's workflow.
fn default_log_file() -> PathBuf {
    PathBuf::from("_running/logs/avalon-server.log")
}

pub fn run(args: ExportArgs) {
    let contents = std::fs::read_to_string(&args.file).unwrap_or_else(|err| {
        eprintln!(
            "failed to read log file {}: {err}",
            args.file.display()
        );
        std::process::exit(1);
    });

    let secrets = collect_present_secrets();

    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();

    if let Some(since) = args.since {
        lines.retain(|line| line_timestamp(line).map(|ts| ts >= since).unwrap_or(true));
    }

    if let Some(tail) = args.tail {
        if lines.len() > tail {
            let start = lines.len() - tail;
            lines = lines.split_off(start);
        }
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    use std::io::Write as _;
    for line in &lines {
        let normalized = export_line(line, &secrets);
        writeln!(out, "{normalized}").expect("failed to write to stdout");
    }
}

/// Best-effort timestamp extraction, used only for `--since` filtering — a
/// line whose timestamp can't be determined is always kept (bounding is a
/// convenience, not a correctness guarantee; see the "never silently
/// dropped" invariant this command is built around).
fn line_timestamp(line: &str) -> Option<OffsetDateTime> {
    let stripped = strip_ansi(line);
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&stripped) {
        if let Some(ts) = map.get("timestamp").and_then(Value::as_str) {
            return OffsetDateTime::parse(ts, &Rfc3339).ok();
        }
    }
    let first_token = stripped.split_whitespace().next()?;
    OffsetDateTime::parse(first_token, &Rfc3339).ok()
}

/// Runs one raw log line through the full pipeline: strip ANSI, redact
/// (exact-value, then shape-based), normalize to a JSON object, serialize.
/// Every input line produces exactly one output line — never zero (that
/// would silently drop it) and never more than one.
fn export_line(raw_line: &str, secrets: &[(String, String)]) -> String {
    let stripped = strip_ansi(raw_line);
    let redacted = redact_shape(&redact_exact(&stripped, secrets));
    let value = normalize(&redacted);
    serde_json::to_string(&value).unwrap_or_else(|_| {
        serde_json::to_string(&json!({ "raw": redacted })).expect("serializing a raw fallback")
    })
}

/// For the human-readable format, `tracing-subscriber`'s fmt layer emits
/// ANSI color codes unconditionally unless told otherwise — `make start`
/// redirects stdout to a plain file, so those codes end up literally in
/// the log text (see `crates/server/src/main.rs`'s `init_tracing`). JSON
/// format never has them, but stripping is a no-op on text with none, so
/// it's safe to always run.
fn strip_ansi(line: &str) -> String {
    let bytes = strip_ansi_escapes::strip(line.as_bytes());
    String::from_utf8(bytes).unwrap_or_else(|_| line.to_string())
}

/// Layer 1 — the "we know what we're looking for" case. For every secret
/// env var this process cares about that happens to be set (non-empty) in
/// *this* exporting shell's own environment, replace every literal
/// occurrence of its value in the line with a fixed placeholder.
///
/// This only catches a secret if the exporting shell has the same value
/// set that the logging process had at the time — e.g. run right after
/// `make start` with the same `.env`, which is the expected normal case.
/// It is not a guarantee against secrets logged under a different value,
/// from a different process, or from a since-rotated key — that's what
/// [`redact_shape`] is for, and even that one is a fallback, not a proof.
fn redact_exact(line: &str, secrets: &[(String, String)]) -> String {
    let mut out = line.to_string();
    for (_name, value) in secrets {
        if value.is_empty() {
            continue;
        }
        out = out.replace(value.as_str(), REDACTED);
    }
    out
}

fn collect_present_secrets() -> Vec<(String, String)> {
    SECRET_ENV_VARS
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| (name.to_string(), value))
        })
        .collect()
}

static POSTGRES_CREDENTIALS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(postgres(?:ql)?)://[^:/@\s]+:[^@/\s]+@").expect("valid regex")
});

static BEARER_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bBearer\s+([A-Za-z0-9\-._~+/]+=*)").expect("valid regex"));

/// Layer 2 — the "catch what didn't have a matching env var set" fallback:
/// generic shapes that are almost always sensitive regardless of which
/// process or key rotation produced them. Only covers the two shapes this
/// project's own log lines are known to be able to contain
/// (`postgres://user:pass@host/db` connection strings, `Bearer <token>`
/// auth headers) — it is not a general secret scanner and should not be
/// treated as one.
///
/// For a Postgres URL, only the `user:pass` portion is redacted — the
/// host/db is left visible since it's ordinarily not sensitive and is
/// useful debugging context in a filed issue.
fn redact_shape(line: &str) -> String {
    let after_pg = POSTGRES_CREDENTIALS_RE.replace_all(line, |caps: &regex::Captures| {
        format!("{}://{REDACTED}@", &caps[1])
    });
    BEARER_TOKEN_RE
        .replace_all(&after_pg, format!("Bearer {REDACTED}"))
        .into_owned()
}

/// Best-effort parse of `tracing-subscriber`'s default human-readable
/// format: `<timestamp>  <LEVEL> <target>: <message + fields>`. Matches
/// against the shape `init_tracing` in `crates/server/src/main.rs` actually
/// produces (`with_ansi` aside, already stripped by this point). Message
/// text and any trailing `field=value` pairs are kept together verbatim
/// under `message` rather than split apart — `tracing`'s field list is
/// free-form enough (arbitrary `Display`/`Debug` values, quoted or not)
/// that trying to separate them reliably risks corrupting content instead
/// of just leaving it alone.
static HUMAN_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<timestamp>\S+)\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR)\s+(?P<target>[^:]+):\s(?P<message>.*)$")
        .expect("valid regex")
});

/// Normalizes one already-redacted line into `{timestamp, level, target,
/// message}`. An already-JSON line is parsed and re-emitted as-is (its
/// keys already match, or at least are already structured, since it came
/// from `AVALON_LOG_FORMAT=json`). A human-readable line is parsed on a
/// best-effort basis into the same shape. Anything that parses as neither
/// is preserved verbatim under `raw` — never silently dropped, per this
/// command's own invariant.
fn normalize(line: &str) -> Value {
    if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(line) {
        return value;
    }

    if let Some(caps) = HUMAN_LINE_RE.captures(line) {
        return json!({
            "timestamp": &caps["timestamp"],
            "level": &caps["level"],
            "target": &caps["target"],
            "message": &caps["message"],
        });
    }

    json!({ "raw": line })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `redact_exact`'s tests mutate real process env vars (by design — the
    // ticket calls for testing against the exporting shell's actual
    // environment) — serialize them so parallel `cargo test` threads don't
    // stomp on each other's env var state.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn redacts_exact_secret_value_from_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var(
            "AVALON_SETTLEMENT_SIGNING_KEY",
            "sekrit-signing-key-abc123",
        );
        let secrets = collect_present_secrets();
        std::env::remove_var("AVALON_SETTLEMENT_SIGNING_KEY");

        let line = "2024-01-01T00:00:00.000000Z  INFO avalon_server: loaded key sekrit-signing-key-abc123 ok";
        let redacted = redact_exact(line, &secrets);

        assert!(!redacted.contains("sekrit-signing-key-abc123"));
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn redacts_exact_value_end_to_end_through_export_line() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("DATABASE_URL", "postgres://realuser:realpass@127.0.0.1/avalon");
        let secrets = collect_present_secrets();
        std::env::remove_var("DATABASE_URL");

        let line = "2024-01-01T00:00:00.000000Z  INFO avalon_server: connected to postgres://realuser:realpass@127.0.0.1/avalon";
        let out = export_line(line, &secrets);

        assert!(!out.contains("realuser:realpass"));
        assert!(!out.contains("postgres://realuser:realpass@127.0.0.1/avalon"));
    }

    #[test]
    fn shape_based_redacts_postgres_credentials_without_matching_env_var() {
        // Deliberately no env var set — this must be caught by the
        // shape-based fallback alone.
        let line = "connection string: postgres://someuser:somepass@db.internal:5432/avalon";
        let redacted = redact_shape(line);

        assert!(!redacted.contains("someuser:somepass"));
        assert!(redacted.contains("db.internal:5432/avalon"));
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn shape_based_redacts_bearer_token_without_matching_env_var() {
        let line = r#"request failed: Authorization: Bearer abc123.def456-ghi_789=="#;
        let redacted = redact_shape(line);

        assert!(!redacted.contains("abc123.def456-ghi_789=="));
        assert!(redacted.contains(&format!("Bearer {REDACTED}")));
    }

    #[test]
    fn unparseable_line_round_trips_into_raw_field() {
        let line = "this is not a tracing log line at all, just noise ###";
        let value = normalize(&strip_ansi(line));

        assert_eq!(value["raw"], line);
        assert!(value.get("timestamp").is_none());
    }

    #[test]
    fn json_line_parses_and_reemits() {
        let line = r#"{"timestamp":"2024-01-01T00:00:00.000000Z","level":"INFO","target":"avalon_server","fields":{"message":"hello"}}"#;
        let value = normalize(line);

        assert_eq!(value["timestamp"], "2024-01-01T00:00:00.000000Z");
        assert_eq!(value["level"], "INFO");
    }

    #[test]
    fn human_readable_line_parses_into_expected_shape() {
        let line = "2024-01-01T00:00:00.123456Z  INFO avalon_server: avalon-server: resolved node roles roles=\"gateway\" settlement_only=false";
        let value = normalize(line);

        assert_eq!(value["timestamp"], "2024-01-01T00:00:00.123456Z");
        assert_eq!(value["level"], "INFO");
        assert_eq!(value["target"], "avalon_server");
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("resolved node roles"));
    }

    #[test]
    fn strips_ansi_codes() {
        let line = "\u{1b}[32mINFO\u{1b}[0m hello world";
        let stripped = strip_ansi(line);
        assert_eq!(stripped, "INFO hello world");
    }

    #[test]
    fn args_parse_positional_file_and_flags() {
        let raw = vec![
            "some/path.log".to_string(),
            "--tail".to_string(),
            "50".to_string(),
        ];
        let parsed = ExportArgs::parse(&raw).unwrap();
        assert_eq!(parsed.file, PathBuf::from("some/path.log"));
        assert_eq!(parsed.tail, Some(50));
        assert!(parsed.since.is_none());
    }

    #[test]
    fn args_default_file_matches_makefile_convention() {
        let parsed = ExportArgs::parse(&[]).unwrap();
        assert_eq!(parsed.file, PathBuf::from("_running/logs/avalon-server.log"));
    }
}
