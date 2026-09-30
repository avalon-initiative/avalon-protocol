//! Host prerequisites of the managed PostgreSQL in `avalon-server-bundled`, checked before it
//! starts (and by `avalon setup`) so a missing package is named instead of surfacing as an
//! opaque Postgres failure. Pure decision logic sits behind [`Probe`] so it is testable
//! without touching the host.
//!
//! Only some findings block the start: running as root, an unresolved library reported by
//! `ldd` on the downloaded PostgreSQL binary (the reliable check), a missing timezone
//! database, and an unreachable download host. Whether a library is present before the
//! download is a best-effort guess and only warns.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Comma-separated prerequisites to report as missing regardless of the host, for testing
/// the preflight messages: `root`, `tzdata`, `network`, `ldd`, or a library stem such as
/// `libxml2`.
pub const SIMULATE_ENV: &str = "AVALON_BUNDLED_PREFLIGHT_SIMULATE_MISSING";

/// Set to any non-empty value other than `0`/`false` to skip every check here.
pub const SKIP_ENV: &str = "AVALON_BUNDLED_SKIP_PREFLIGHT";

const DOWNLOAD_HOST: &str = "github.com";
const DOWNLOAD_PORT: u16 = 443;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_DEADLINE: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Root,
    /// A shared library, by soname prefix (for example `libxml2`).
    Library(String),
    Tzdata,
    Network,
    /// `ldd` is not installed, so the downloaded binary's libraries cannot be checked.
    LddMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Blocking,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: Kind,
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    pub fn is_blocking(&self) -> bool {
        self.severity == Severity::Blocking
    }
}

/// Outcome of the download-host probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Reachable,
    Unreachable,
    /// A proxy is configured, so a direct connection test would prove nothing.
    ProxySkipped,
}

/// Facts about the host, injectable for tests.
pub trait Probe {
    fn is_root(&self) -> bool;
    fn has_library(&self, name: &str) -> bool;
    fn has_tzdata(&self) -> bool;
    fn download_host(&self) -> Reach;
    /// Unresolved shared-library names of an installed PostgreSQL binary; `None` when `ldd` is
    /// not available.
    fn unresolved_libraries(&self, postgres: &Path) -> Option<Vec<String>>;
}

/// Returns every unmet prerequisite. `postgres_binary` is the already-installed server binary,
/// if any; when it is absent a first-run download is needed and reachability is checked.
pub fn check(probe: &dyn Probe, postgres_binary: Option<&Path>) -> Vec<Problem> {
    let mut problems = Vec::new();
    if probe.is_root() {
        problems.push(Problem {
            kind: Kind::Root,
            severity: Severity::Blocking,
            message: "running as root: PostgreSQL refuses to start as root. Run the node as an \
                      ordinary user (for example a dedicated `avalon` account)."
                .to_string(),
        });
    }
    match postgres_binary {
        Some(bin) => match probe.unresolved_libraries(bin) {
            Some(missing) => {
                for lib in missing {
                    problems.push(library_problem(&lib, Severity::Blocking));
                }
            }
            None => problems.push(Problem {
                kind: Kind::LddMissing,
                severity: Severity::Warning,
                message: "the `ldd` tool was not found, so the shared libraries of the \
                          downloaded PostgreSQL could not be checked in advance (it is provided \
                          by libc-bin on Debian/Ubuntu and glibc-common on Fedora/RHEL); a \
                          missing library will show up when PostgreSQL starts."
                    .to_string(),
            }),
        },
        None => {
            if !probe.has_library("libxml2.so.2") {
                problems.push(library_problem("libxml2.so.2", Severity::Warning));
            }
        }
    }
    if !probe.has_tzdata() {
        problems.push(Problem {
            kind: Kind::Tzdata,
            severity: Severity::Blocking,
            message: format!(
                "no timezone database found (no zoneinfo/UTC under TZDIR or /usr/share/zoneinfo): \
                 PostgreSQL will fail with `invalid value for parameter \"TimeZone\"`. Install the \
                 tzdata package:\n{}",
                install_hint("tzdata", "tzdata")
            ),
        });
    }
    if postgres_binary.is_none() {
        match probe.download_host() {
            Reach::Reachable => {}
            Reach::ProxySkipped => problems.push(Problem {
                kind: Kind::Network,
                severity: Severity::Warning,
                message: format!(
                    "the first start downloads PostgreSQL from https://{DOWNLOAD_HOST}; the \
                     reachability check was skipped because a proxy (HTTPS_PROXY/ALL_PROXY) is \
                     configured."
                ),
            }),
            Reach::Unreachable => problems.push(Problem {
                kind: Kind::Network,
                severity: Severity::Blocking,
                message: format!(
                    "the first start downloads PostgreSQL from https://{DOWNLOAD_HOST}, which is \
                     not reachable on port {DOWNLOAD_PORT} within {}s. Allow outbound HTTPS (or \
                     set HTTPS_PROXY), or run avalon-server with your own PostgreSQL through \
                     DATABASE_URL.",
                    PROBE_DEADLINE.as_secs()
                ),
            }),
        }
    }
    problems
}

fn library_problem(soname: &str, severity: Severity) -> Problem {
    let stem = soname.split(".so").next().unwrap_or(soname).to_string();
    let how = match package_for_library(soname) {
        Some((apt, rpm)) => format!("Install the {apt} package:\n{}", install_hint(&apt, &rpm)),
        None => format!(
            "Install the package that provides it (find it with `apt-file search {soname}` on \
             Debian/Ubuntu or `dnf provides '*/{soname}'` on Fedora/RHEL)."
        ),
    };
    let message = match severity {
        Severity::Blocking => {
            format!(
                "the shared library {soname} needed by the embedded PostgreSQL is missing. {how}"
            )
        }
        Severity::Warning => format!(
            "could not confirm the shared library {soname} needed by the embedded PostgreSQL \
             (checked ldconfig and the standard library directories); if the start fails with a \
             loading error, {}",
            lowercase_first(&how)
        ),
    };
    Problem {
        kind: Kind::Library(stem),
        severity,
        message,
    }
}

fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Package names (Debian family, RPM family) for a soname, or `None` when the mapping would be
/// a guess.
fn package_for_library(soname: &str) -> Option<(String, String)> {
    let stem = soname.split(".so").next().unwrap_or(soname);
    let version = soname.split(".so.").nth(1).unwrap_or_default();
    let pair = |a: &str, r: &str| Some((a.to_string(), r.to_string()));
    match (stem, version) {
        ("libxml2", _) => pair("libxml2", "libxml2"),
        ("libz", "1") => pair("zlib1g", "zlib"),
        ("libssl" | "libcrypto", v) if !v.is_empty() => {
            Some((format!("libssl{v}"), "openssl-libs".to_string()))
        }
        ("libicuuc" | "libicui18n" | "libicudata", v) if !v.is_empty() => {
            Some((format!("libicu{v}"), "libicu".to_string()))
        }
        ("liblz4", "1") => pair("liblz4-1", "lz4-libs"),
        ("libzstd", "1") => pair("libzstd1", "libzstd"),
        ("liblzma", "5") => pair("liblzma5", "xz-libs"),
        ("libgssapi_krb5", "2") => pair("libgssapi-krb5-2", "krb5-libs"),
        ("libkrb5", "3") => pair("libkrb5-3", "krb5-libs"),
        ("libk5crypto", "3") => pair("libk5crypto3", "krb5-libs"),
        ("libkrb5support", "0") => pair("libkrb5support0", "krb5-libs"),
        ("libstdc++", "6") => pair("libstdc++6", "libstdc++"),
        _ => None,
    }
}

/// The install command for the common distro families.
pub fn install_hint(apt_package: &str, rpm_package: &str) -> String {
    format!(
        "  Debian/Ubuntu:  sudo apt-get install -y {apt_package}\n  \
         Fedora/RHEL:    sudo dnf install -y {rpm_package}\n  \
         openSUSE:       sudo zypper install -y {rpm_package}"
    )
}

/// Renders problems for a terminal or log line.
pub fn format_problems(problems: &[Problem]) -> String {
    let mut out = if problems.iter().any(Problem::is_blocking) {
        String::from("the bundled PostgreSQL prerequisites are not met:\n")
    } else {
        String::from("bundled PostgreSQL prerequisite warnings:\n")
    };
    for p in problems {
        out.push_str(if p.is_blocking() {
            "- "
        } else {
            "- (warning) "
        });
        out.push_str(&p.message);
        out.push('\n');
    }
    out
}

/// Server binary under `install_dir` (`<install_dir>/<version>/bin/postgres` or
/// `<install_dir>/bin/postgres`), if PostgreSQL has already been downloaded.
pub fn installed_postgres(install_dir: &Path) -> Option<PathBuf> {
    let direct = install_dir.join("bin").join("postgres");
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(install_dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join("bin").join("postgres"))
        .find(|p| p.is_file())
}

/// Parses `ldd` output into the sonames it reports as `not found`.
pub fn parse_ldd_not_found(ldd_output: &str) -> Vec<String> {
    ldd_output
        .lines()
        .filter(|l| l.contains("not found"))
        .filter_map(|l| l.split("=>").next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether `ldconfig -p` output lists a library named `name`.
pub fn ldconfig_lists(output: &str, name: &str) -> bool {
    output
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .any(|first| first == name)
}

/// Recognizes a known PostgreSQL startup failure in its output and returns the hint for it.
pub fn explain_failure(output: &str) -> Option<Problem> {
    if let Some(soname) = shared_library_failure(output) {
        return Some(library_problem(&soname, Severity::Blocking));
    }
    if output.contains("parameter \"TimeZone\"") || output.contains("timezone directory") {
        return Some(Problem {
            kind: Kind::Tzdata,
            severity: Severity::Blocking,
            message: format!(
                "PostgreSQL rejected the timezone setting because the host has no timezone \
                 database. Install the tzdata package:\n{}",
                install_hint("tzdata", "tzdata")
            ),
        });
    }
    None
}

/// The soname in `error while loading shared libraries: <soname>: cannot open shared object file`.
fn shared_library_failure(output: &str) -> Option<String> {
    let marker = "error while loading shared libraries: ";
    let rest = &output[output.find(marker)? + marker.len()..];
    let soname = rest.split(':').next()?.trim();
    (!soname.is_empty()).then(|| soname.to_string())
}

/// The last `lines` non-empty lines of `text`.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// The real host, with the [`SIMULATE_ENV`] overrides applied.
pub struct SystemProbe {
    pub is_root: bool,
    simulated: Vec<String>,
}

impl SystemProbe {
    pub fn new(is_root: bool) -> Self {
        let simulated = std::env::var(SIMULATE_ENV)
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        Self { is_root, simulated }
    }

    fn simulating(&self, what: &str) -> bool {
        self.simulated.iter().any(|s| s == what)
    }
}

fn library_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/local/lib"]
        .iter()
        .map(PathBuf::from)
        .collect();
    for base in ["/lib", "/usr/lib"] {
        if let Ok(entries) = std::fs::read_dir(base) {
            dirs.extend(
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with("-linux-gnu"))
                    .map(|e| e.path()),
            );
        }
    }
    if let Ok(paths) = std::env::var("LD_LIBRARY_PATH") {
        dirs.extend(
            paths
                .split(':')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        );
    }
    dirs
}

fn tz_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(d) = std::env::var("TZDIR") {
        dirs.push(PathBuf::from(d));
    }
    dirs.extend(
        [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/usr/share/lib/zoneinfo",
        ]
        .iter()
        .map(PathBuf::from),
    );
    dirs
}

/// First existing tool among the fixed system directories.
fn system_tool(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

fn ldconfig_output() -> Option<String> {
    let tool = system_tool("ldconfig")?;
    let out = std::process::Command::new(tool).arg("-p").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Connects to the download host on a helper thread so DNS and connect share one deadline.
fn probe_https() -> bool {
    use std::net::ToSocketAddrs;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = (DOWNLOAD_HOST, DOWNLOAD_PORT)
            .to_socket_addrs()
            .map(|addrs| {
                addrs
                    .into_iter()
                    .any(|a| std::net::TcpStream::connect_timeout(&a, CONNECT_TIMEOUT).is_ok())
            })
            .unwrap_or(false);
        let _ = tx.send(ok);
    });
    rx.recv_timeout(PROBE_DEADLINE).unwrap_or(false)
}

impl Probe for SystemProbe {
    fn is_root(&self) -> bool {
        self.is_root || self.simulating("root")
    }

    fn has_library(&self, name: &str) -> bool {
        if self.simulating(name.split(".so").next().unwrap_or(name)) {
            return false;
        }
        let in_dirs = library_dirs().iter().any(|dir| {
            std::fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with(name))
            })
        });
        in_dirs || ldconfig_output().is_some_and(|out| ldconfig_lists(&out, name))
    }

    fn has_tzdata(&self) -> bool {
        !self.simulating("tzdata") && tz_dirs().iter().any(|d| d.join("UTC").is_file())
    }

    fn download_host(&self) -> Reach {
        if self.simulating("network") {
            return Reach::Unreachable;
        }
        if ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
        {
            return Reach::ProxySkipped;
        }
        if probe_https() {
            Reach::Reachable
        } else {
            Reach::Unreachable
        }
    }

    fn unresolved_libraries(&self, postgres: &Path) -> Option<Vec<String>> {
        let mut missing: Vec<String> = self
            .simulated
            .iter()
            .filter(|s| !["root", "tzdata", "network", "ldd"].contains(&s.as_str()))
            .map(|s| format!("{s}.so"))
            .collect();
        if self.simulating("ldd") {
            return None;
        }
        let ldd = system_tool("ldd")?;
        let out = std::process::Command::new(ldd)
            .arg(postgres)
            .output()
            .ok()?;
        missing.extend(parse_ldd_not_found(&String::from_utf8_lossy(&out.stdout)));
        Some(missing)
    }
}

/// True when [`SKIP_ENV`] asks to skip the checks.
pub fn skipped() -> bool {
    std::env::var(SKIP_ENV)
        .map(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// Checks the host against the bundled variant's prerequisites, given where PostgreSQL would
/// be (or already is) installed. Empty when [`SKIP_ENV`] is set.
pub fn check_host(is_root: bool, install_dir: &Path) -> Vec<Problem> {
    if skipped() {
        return Vec::new();
    }
    let installed = installed_postgres(install_dir);
    check(&SystemProbe::new(is_root), installed.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        root: bool,
        libs: Vec<&'static str>,
        no_tz: bool,
        reach: Reach,
        unresolved: Option<Vec<String>>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                root: false,
                libs: Vec::new(),
                no_tz: false,
                reach: Reach::Reachable,
                unresolved: Some(Vec::new()),
            }
        }
    }

    impl Probe for Fake {
        fn is_root(&self) -> bool {
            self.root
        }
        fn has_library(&self, name: &str) -> bool {
            self.libs.contains(&name)
        }
        fn has_tzdata(&self) -> bool {
            !self.no_tz
        }
        fn download_host(&self) -> Reach {
            self.reach
        }
        fn unresolved_libraries(&self, _: &Path) -> Option<Vec<String>> {
            self.unresolved.clone()
        }
    }

    fn healthy() -> Fake {
        Fake {
            libs: vec!["libxml2.so.2"],
            ..Fake::default()
        }
    }

    const BIN: &str = "/x/bin/postgres";

    #[test]
    fn healthy_host_has_no_problems() {
        assert!(check(&healthy(), None).is_empty());
        assert!(check(&healthy(), Some(Path::new(BIN))).is_empty());
    }

    #[test]
    fn unconfirmed_libxml2_before_download_only_warns() {
        let p = check(&Fake::default(), None);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, Kind::Library("libxml2".into()));
        assert_eq!(p[0].severity, Severity::Warning);
        assert!(p[0].message.contains("apt-get install -y libxml2"));
        assert!(p[0].message.contains("dnf install -y libxml2"));
        assert!(!format_problems(&p).contains("are not met"));
    }

    #[test]
    fn unresolved_library_after_download_blocks() {
        let probe = Fake {
            unresolved: Some(vec!["libxml2.so.2".to_string()]),
            ..healthy()
        };
        let p = check(&probe, Some(Path::new(BIN)));
        assert_eq!(p.len(), 1);
        assert!(p[0].is_blocking());
        assert!(format_problems(&p).contains("are not met"));
    }

    #[test]
    fn missing_ldd_is_reported_not_skipped_silently() {
        let probe = Fake {
            unresolved: None,
            ..healthy()
        };
        let p = check(&probe, Some(Path::new(BIN)));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, Kind::LddMissing);
        assert_eq!(p[0].severity, Severity::Warning);
        assert!(p[0].message.contains("libc-bin"));
    }

    #[test]
    fn missing_tzdata_blocks() {
        let p = check(
            &Fake {
                no_tz: true,
                ..healthy()
            },
            None,
        );
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, Kind::Tzdata);
        assert!(p[0].is_blocking());
        assert!(p[0].message.contains("apt-get install -y tzdata"));
    }

    #[test]
    fn root_blocks() {
        let p = check(
            &Fake {
                root: true,
                ..healthy()
            },
            None,
        );
        assert_eq!(p[0].kind, Kind::Root);
        assert!(p[0].is_blocking());
    }

    #[test]
    fn network_only_matters_when_a_download_is_needed() {
        let offline = Fake {
            reach: Reach::Unreachable,
            ..healthy()
        };
        assert!(check(&offline, Some(Path::new(BIN))).is_empty());
        let p = check(&offline, None);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, Kind::Network);
        assert!(p[0].is_blocking());
    }

    #[test]
    fn a_proxy_skips_the_probe_and_says_so() {
        let proxied = Fake {
            reach: Reach::ProxySkipped,
            ..healthy()
        };
        let p = check(&proxied, None);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].severity, Severity::Warning);
        assert!(p[0].message.contains("skipped"));
    }

    #[test]
    fn installed_binary_uses_unresolved_libraries_not_the_libxml2_guess() {
        let probe = Fake {
            unresolved: Some(vec![
                "libxml2.so.2".to_string(),
                "libicuuc.so.72".to_string(),
            ]),
            ..Fake::default()
        };
        let p = check(&probe, Some(Path::new(BIN)));
        assert_eq!(p.len(), 2);
        assert!(p[1].message.contains("apt-get install -y libicu72"));
    }

    #[test]
    fn ssl_package_follows_the_soname_version() {
        assert_eq!(
            package_for_library("libssl.so.3"),
            Some(("libssl3".to_string(), "openssl-libs".to_string()))
        );
        assert_eq!(
            package_for_library("libcrypto.so.1.1"),
            Some(("libssl1.1".to_string(), "openssl-libs".to_string()))
        );
    }

    #[test]
    fn unknown_libraries_get_a_search_hint_not_a_guessed_package() {
        assert_eq!(package_for_library("libfoo.so.9"), None);
        assert_eq!(package_for_library("libz.so.2"), None);
        let p = library_problem("libfoo.so.9", Severity::Blocking);
        assert!(p.message.contains("apt-file search libfoo.so.9"));
        assert!(!p.message.contains("apt-get install -y libfoo"));
    }

    #[test]
    fn ldd_output_parsing() {
        let out = "\tlinux-vdso.so.1 (0x00007ffd)\n\tlibxml2.so.2 => not found\n\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1)\n";
        assert_eq!(parse_ldd_not_found(out), vec!["libxml2.so.2".to_string()]);
    }

    #[test]
    fn ldconfig_cache_lookup() {
        let out = "2 libs found in cache\n\tlibxml2.so.2 (libc6,x86-64) => /opt/x/libxml2.so.2\n\tlibz.so.1 (libc6,x86-64) => /lib/libz.so.1\n";
        assert!(ldconfig_lists(out, "libxml2.so.2"));
        assert!(!ldconfig_lists(out, "libxml2.so"));
    }

    #[test]
    fn known_startup_failures_map_to_hints() {
        let lib = explain_failure(
            "postgres: error while loading shared libraries: libxml2.so.2: cannot open shared object file: No such file or directory",
        )
        .unwrap();
        assert_eq!(lib.kind, Kind::Library("libxml2".into()));
        assert!(lib.message.contains("apt-get install -y libxml2"));

        let tz = explain_failure("FATAL:  invalid value for parameter \"TimeZone\": \"Etc/UTC\"")
            .unwrap();
        assert_eq!(tz.kind, Kind::Tzdata);

        assert!(explain_failure("FATAL: could not create shared memory segment").is_none());
    }

    #[test]
    fn skip_env_disables_every_check() {
        let _guard = crate::test_env::guard();
        std::env::set_var(SKIP_ENV, "1");
        std::env::set_var(SIMULATE_ENV, "root,tzdata,network");
        let skipped_result = check_host(true, Path::new("/nonexistent"));
        std::env::remove_var(SKIP_ENV);
        let checked_result = check_host(true, Path::new("/nonexistent"));
        std::env::remove_var(SIMULATE_ENV);
        assert!(skipped_result.is_empty());
        assert!(checked_result.iter().any(|p| p.kind == Kind::Root));
    }

    #[test]
    fn tail_keeps_the_last_non_empty_lines() {
        assert_eq!(tail("a\n\nb\nc\nd\n", 2), "c\nd");
        assert_eq!(tail("a", 5), "a");
    }

    #[test]
    fn installed_postgres_finds_versioned_layout() {
        let dir = std::env::temp_dir().join(format!("avalon-prereq-{}", std::process::id()));
        let bin = dir.join("17.5.0").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert!(installed_postgres(&dir).is_none());
        std::fs::write(bin.join("postgres"), b"").unwrap();
        assert_eq!(installed_postgres(&dir), Some(bin.join("postgres")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
