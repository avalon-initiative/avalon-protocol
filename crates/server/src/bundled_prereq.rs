//! Host prerequisites of the managed PostgreSQL in `avalon-server-bundled`, checked before it
//! starts (and by `avalon setup`) so a missing package is named instead of surfacing as an
//! opaque Postgres failure. Pure decision logic sits behind [`Probe`] so it is testable
//! without touching the host.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Comma-separated prerequisites to report as missing regardless of the host, for testing
/// the preflight messages: any of `root`, `libxml2`, `tzdata`, `network`.
pub const SIMULATE_ENV: &str = "AVALON_BUNDLED_PREFLIGHT_SIMULATE_MISSING";

const DOWNLOAD_HOST: &str = "github.com";
const DOWNLOAD_PORT: u16 = 443;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Root,
    /// A shared library, by soname prefix (for example `libxml2`).
    Library(String),
    Tzdata,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: Kind,
    pub message: String,
}

/// Facts about the host, injectable for tests.
pub trait Probe {
    fn is_root(&self) -> bool;
    fn has_library(&self, name: &str) -> bool;
    fn has_tzdata(&self) -> bool;
    fn https_reachable(&self) -> bool;
    /// Unresolved shared-library names of an installed PostgreSQL binary, when there is one.
    fn unresolved_libraries(&self, postgres: &Path) -> Vec<String>;
}

/// Returns every unmet prerequisite. `postgres_binary` is the already-installed server binary,
/// if any; when it is absent a first-run download is needed and reachability is checked.
pub fn check(probe: &dyn Probe, postgres_binary: Option<&Path>) -> Vec<Problem> {
    let mut problems = Vec::new();
    if probe.is_root() {
        problems.push(Problem {
            kind: Kind::Root,
            message: "running as root: PostgreSQL refuses to start as root. Run the node as an \
                      ordinary user (for example a dedicated `avalon` account)."
                .to_string(),
        });
    }
    match postgres_binary {
        Some(bin) => {
            for lib in probe.unresolved_libraries(bin) {
                problems.push(library_problem(&lib));
            }
        }
        None => {
            if !probe.has_library("libxml2.so.2") {
                problems.push(library_problem("libxml2.so.2"));
            }
        }
    }
    if !probe.has_tzdata() {
        problems.push(Problem {
            kind: Kind::Tzdata,
            message: format!(
                "no timezone database found (no /usr/share/zoneinfo/UTC): PostgreSQL will fail \
                 with `invalid value for parameter \"TimeZone\"`. Install the tzdata package:\n{}",
                install_hint("tzdata", "tzdata")
            ),
        });
    }
    if postgres_binary.is_none() && !probe.https_reachable() {
        problems.push(Problem {
            kind: Kind::Network,
            message: format!(
                "the first start downloads PostgreSQL from https://{DOWNLOAD_HOST}, which is not \
                 reachable on port {DOWNLOAD_PORT}. Allow outbound HTTPS (or set HTTPS_PROXY), \
                 or run avalon-server with your own PostgreSQL through DATABASE_URL."
            ),
        });
    }
    problems
}

fn library_problem(soname: &str) -> Problem {
    let stem = soname.split(".so").next().unwrap_or(soname).to_string();
    let (apt, rpm) = package_for_library(soname);
    Problem {
        kind: Kind::Library(stem.clone()),
        message: format!(
            "the shared library {soname} needed by the embedded PostgreSQL is missing. \
             Install the {apt} package:\n{}",
            install_hint(&apt, &rpm)
        ),
    }
}

/// Package names (Debian family, RPM family) for the libraries the embedded PostgreSQL loads.
fn package_for_library(soname: &str) -> (String, String) {
    let stem = soname.split(".so").next().unwrap_or(soname);
    let (apt, rpm): (String, &str) = match stem {
        "libxml2" => ("libxml2".into(), "libxml2"),
        "libz" => ("zlib1g".into(), "zlib"),
        "libssl" | "libcrypto" => ("libssl3".into(), "openssl-libs"),
        "libicuuc" | "libicui18n" | "libicudata" => {
            let version = soname.rsplit('.').next().unwrap_or_default();
            (format!("libicu{version}"), "libicu")
        }
        "liblz4" => ("liblz4-1".into(), "lz4-libs"),
        "libzstd" => ("libzstd1".into(), "libzstd"),
        "liblzma" => ("liblzma5".into(), "xz-libs"),
        "libgssapi_krb5" | "libkrb5" | "libk5crypto" | "libkrb5support" => {
            ("libgssapi-krb5-2".into(), "krb5-libs")
        }
        "libstdc++" => ("libstdc++6".into(), "libstdc++"),
        _ => (stem.to_string(), stem),
    };
    (apt, rpm.to_string())
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
    let mut out = String::from("the bundled PostgreSQL prerequisites are not met:\n");
    for p in problems {
        out.push_str("- ");
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

/// Recognizes a known PostgreSQL startup failure in its output and returns the hint for it.
pub fn explain_failure(output: &str) -> Option<Problem> {
    if let Some(soname) = shared_library_failure(output) {
        return Some(library_problem(&soname));
    }
    if output.contains("parameter \"TimeZone\"") || output.contains("timezone directory") {
        return check_tz_problem();
    }
    None
}

fn check_tz_problem() -> Option<Problem> {
    Some(Problem {
        kind: Kind::Tzdata,
        message: format!(
            "PostgreSQL rejected the timezone setting because the host has no timezone \
             database. Install the tzdata package:\n{}",
            install_hint("tzdata", "tzdata")
        ),
    })
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

impl Probe for SystemProbe {
    fn is_root(&self) -> bool {
        self.is_root || self.simulating("root")
    }

    fn has_library(&self, name: &str) -> bool {
        if self.simulating(name.split(".so").next().unwrap_or(name)) {
            return false;
        }
        library_dirs().iter().any(|dir| {
            std::fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with(name))
            })
        })
    }

    fn has_tzdata(&self) -> bool {
        !self.simulating("tzdata") && tz_dirs().iter().any(|d| d.join("UTC").is_file())
    }

    fn https_reachable(&self) -> bool {
        if self.simulating("network") {
            return false;
        }
        // A proxy is in use: a direct connection test would give a false negative.
        if ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
        {
            return true;
        }
        use std::net::ToSocketAddrs;
        (DOWNLOAD_HOST, DOWNLOAD_PORT)
            .to_socket_addrs()
            .map(|addrs| {
                addrs.into_iter().any(|a| {
                    std::net::TcpStream::connect_timeout(&a, Duration::from_secs(5)).is_ok()
                })
            })
            .unwrap_or(false)
    }

    fn unresolved_libraries(&self, postgres: &Path) -> Vec<String> {
        let mut missing = Vec::new();
        for sim in &self.simulated {
            if sim != "root" && sim != "tzdata" && sim != "network" {
                missing.push(format!("{sim}.so"));
            }
        }
        let ldd = ["/usr/bin/ldd", "/bin/ldd"]
            .iter()
            .map(Path::new)
            .find(|p| p.is_file());
        if let Some(ldd) = ldd {
            if let Ok(out) = std::process::Command::new(ldd).arg(postgres).output() {
                missing.extend(parse_ldd_not_found(&String::from_utf8_lossy(&out.stdout)));
            }
        }
        missing
    }
}

/// Checks the host against the bundled variant's prerequisites, given where PostgreSQL would
/// be (or already is) installed.
pub fn check_host(is_root: bool, install_dir: &Path) -> Vec<Problem> {
    let installed = installed_postgres(install_dir);
    check(&SystemProbe::new(is_root), installed.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        root: bool,
        libs: Vec<&'static str>,
        no_tz: bool,
        no_net: bool,
        unresolved: Vec<String>,
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
        fn https_reachable(&self) -> bool {
            !self.no_net
        }
        fn unresolved_libraries(&self, _: &Path) -> Vec<String> {
            self.unresolved.clone()
        }
    }

    fn healthy() -> Fake {
        Fake {
            libs: vec!["libxml2.so.2"],
            ..Fake::default()
        }
    }

    #[test]
    fn healthy_host_has_no_problems() {
        assert!(check(&healthy(), None).is_empty());
        assert!(check(&healthy(), Some(Path::new("/x/bin/postgres"))).is_empty());
    }

    #[test]
    fn missing_libxml2_names_the_package_and_commands() {
        let p = check(&Fake::default(), None);
        let lib = p
            .iter()
            .find(|p| p.kind == Kind::Library("libxml2".into()))
            .expect("libxml2 problem");
        assert!(lib.message.contains("libxml2"));
        assert!(lib.message.contains("apt-get install -y libxml2"));
        assert!(lib.message.contains("dnf install -y libxml2"));
    }

    #[test]
    fn missing_tzdata_is_reported() {
        let p = check(
            &Fake {
                no_tz: true,
                ..healthy()
            },
            None,
        );
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, Kind::Tzdata);
        assert!(p[0].message.contains("apt-get install -y tzdata"));
    }

    #[test]
    fn root_is_reported() {
        let p = check(
            &Fake {
                root: true,
                ..healthy()
            },
            None,
        );
        assert_eq!(p[0].kind, Kind::Root);
    }

    #[test]
    fn network_only_matters_when_a_download_is_needed() {
        let offline = Fake {
            no_net: true,
            ..healthy()
        };
        assert!(check(&offline, Some(Path::new("/x/bin/postgres"))).is_empty());
        let p = check(&offline, None);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, Kind::Network);
    }

    #[test]
    fn installed_binary_uses_unresolved_libraries_not_the_libxml2_guess() {
        let probe = Fake {
            unresolved: vec!["libxml2.so.2".to_string(), "libicuuc.so.72".to_string()],
            ..Fake::default()
        };
        let p = check(&probe, Some(Path::new("/x/bin/postgres")));
        assert_eq!(p.len(), 2);
        assert!(p[1].message.contains("apt-get install -y libicu72"));
    }

    #[test]
    fn ldd_output_parsing() {
        let out = "\tlinux-vdso.so.1 (0x00007ffd)\n\tlibxml2.so.2 => not found\n\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1)\n";
        assert_eq!(parse_ldd_not_found(out), vec!["libxml2.so.2".to_string()]);
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
