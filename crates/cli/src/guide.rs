//! `avalon guide [topic]`: the hosting guide, embedded from the published docs at build time.

const README: &str = include_str!("../../../docs/projects/backend-server/for-hosters/README.md");
const STANDALONE: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/standalone-binary.md");
const QUICKSTART: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/hosting-quickstart.md");
const DEPLOYMENT: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/deployment.md");
const UPGRADING: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/upgrading.md");
const SHARDS: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/choosing-your-shard.md");
const SEED_NODES: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/seed-nodes.md");
const VERIFY_RELEASE: &str =
    include_str!("../../../docs/projects/backend-server/for-hosters/verifying-a-release.md");

/// Where a topic's text comes from: a whole document, or sections of the standalone guide.
enum Source {
    Whole(&'static str),
    Section(&'static str),
    /// A `###` subsection anywhere in the standalone guide.
    Subsection(&'static str),
    /// A `##` section followed by a `###` subsection of the bundled-variant section.
    SectionAndBundledSub(&'static str, &'static str),
}

struct Topic {
    name: &'static str,
    summary: &'static str,
    source: Source,
}

const TOPICS: &[Topic] = &[
    Topic {
        name: "index",
        summary: "the list of hosting guides",
        source: Source::Whole(README),
    },
    Topic {
        name: "standalone",
        summary: "the whole standalone-binary guide",
        source: Source::Whole(STANDALONE),
    },
    Topic {
        name: "configuration",
        summary: "environment variables, replica and authoring nodes",
        source: Source::Section("Configuration"),
    },
    Topic {
        name: "networks",
        summary: "joining a network through its seed nodes",
        source: Source::Subsection("Connecting to a network"),
    },
    Topic {
        name: "first-start",
        summary: "what the first start logs and how to check it",
        source: Source::Section("First start"),
    },
    Topic {
        name: "ports",
        summary: "the listeners other nodes must reach",
        source: Source::Section("Ports"),
    },
    Topic {
        name: "systemd",
        summary: "running the node as a systemd service",
        source: Source::Section("Running under systemd"),
    },
    Topic {
        name: "upgrading",
        summary: "replacing the binary and applying migrations",
        source: Source::Section("Upgrading"),
    },
    Topic {
        name: "backups",
        summary: "what to back up, for both binary variants",
        source: Source::SectionAndBundledSub("Backup", "Backup"),
    },
    Topic {
        name: "bundled",
        summary: "the avalon-server-bundled variant",
        source: Source::Section("Bundled variant"),
    },
    Topic {
        name: "troubleshooting",
        summary: "first-run errors and their fixes",
        source: Source::Section("Troubleshooting"),
    },
    Topic {
        name: "quickstart",
        summary: "the Docker route",
        source: Source::Whole(QUICKSTART),
    },
    Topic {
        name: "tls",
        summary: "TLS termination and production values",
        source: Source::Whole(DEPLOYMENT),
    },
    Topic {
        name: "rollouts",
        summary: "upgrade rollouts and rollback (Docker layout)",
        source: Source::Whole(UPGRADING),
    },
    Topic {
        name: "shards",
        summary: "which shard a node authors",
        source: Source::Whole(SHARDS),
    },
    Topic {
        name: "seed-nodes",
        summary: "running and monitoring seed nodes",
        source: Source::Whole(SEED_NODES),
    },
    Topic {
        name: "verify-release",
        summary: "checking a release download",
        source: Source::Whole(VERIFY_RELEASE),
    },
];

/// The body of the `heading`-titled section at `level` (number of `#`), up to the next heading of
/// the same or a shallower level. Fenced code blocks are skipped when looking for headings.
fn section(doc: &str, level: usize, heading: &str) -> Option<String> {
    let wanted = format!("{} {heading}", "#".repeat(level));
    let mut out: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut capturing = false;
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence {
            if let Some(depth) = heading_depth(line) {
                if capturing && depth <= level {
                    break;
                }
                if !capturing && depth == level && line.trim_end() == wanted {
                    capturing = true;
                }
            }
        }
        if capturing {
            out.push(line);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n").trim_end().to_string())
    }
}

fn heading_depth(line: &str) -> Option<usize> {
    let depth = line.chars().take_while(|c| *c == '#').count();
    (depth > 0 && line[depth..].starts_with(' ')).then_some(depth)
}

fn render(source: &Source) -> Option<String> {
    match source {
        Source::Whole(text) => Some(text.trim_end().to_string()),
        Source::Section(h) => section(STANDALONE, 2, h),
        Source::Subsection(h) => section(STANDALONE, 3, h),
        Source::SectionAndBundledSub(h, sub) => {
            let top = section(STANDALONE, 2, h)?;
            let bundled = section(STANDALONE, 2, "Bundled variant")?;
            let extra = section(&bundled, 3, sub)?;
            Some(format!("{top}\n\n{extra}"))
        }
    }
}

/// The text for `topic`, or the topic list when `topic` is `None`.
pub fn text(topic: Option<&str>) -> Result<String, String> {
    let Some(name) = topic else {
        return Ok(topic_list());
    };
    let name = name.to_ascii_lowercase();
    match TOPICS.iter().find(|t| t.name == name) {
        Some(t) => {
            render(&t.source).ok_or_else(|| format!("guide topic {name} is missing from the docs"))
        }
        None => Err(format!("unknown guide topic {name:?}\n\n{}", topic_list())),
    }
}

pub fn topic_list() -> String {
    let mut out = String::from("usage: avalon guide [topic]\n\ntopics:\n");
    for t in TOPICS {
        out.push_str(&format!("  {:<16} {}\n", t.name, t.summary));
    }
    out.push_str("\nStart with `avalon setup`; `avalon guide standalone` prints the full guide.");
    out
}

pub fn run(args: &[String]) {
    match text(args.first().map(String::as_str)) {
        Ok(t) => println!("{t}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_renders_non_empty() {
        for t in TOPICS {
            let body = render(&t.source).unwrap_or_else(|| panic!("topic {} missing", t.name));
            assert!(body.len() > 40, "topic {} is empty", t.name);
        }
    }

    #[test]
    fn backups_covers_both_variants() {
        let t = text(Some("backups")).unwrap();
        assert!(t.contains("pg_dump"));
        assert!(t.contains("as a whole"));
    }

    #[test]
    fn section_ignores_headings_inside_fences() {
        let doc = "## A\nx\n```\n## not a heading\n```\ny\n## B\nz";
        assert_eq!(
            section(doc, 2, "A").unwrap(),
            "## A\nx\n```\n## not a heading\n```\ny"
        );
    }

    #[test]
    fn unknown_topic_lists_topics() {
        let e = text(Some("nope")).unwrap_err();
        assert!(e.contains("topics:"));
    }
}
