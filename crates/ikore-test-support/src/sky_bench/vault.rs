use super::{Corpus, Fact, Result, fixtures, hash};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use std::{collections::BTreeSet, process::Command};
use walkdir::WalkDir;

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let e = entry?;
        if e.file_type().is_file() {
            files.push(e.path().strip_prefix(root)?.to_owned());
        }
    }
    files.sort();
    Ok(files)
}
pub fn tree_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    files(root)?
        .into_iter()
        .map(|p| {
            Ok((
                p.to_string_lossy().replace('\\', "/"),
                hash(&fs::read(root.join(p))?),
            ))
        })
        .collect()
}
pub(super) fn write(root: &Path, path: &str, body: &str) -> Result<()> {
    let p = root.join(path);
    fs::create_dir_all(p.parent().ok_or("missing parent directory")?)?;
    Ok(fs::write(p, body)?)
}
fn git(root: &Path, args: &[&str], sequence: usize) -> Result<()> {
    let date = format!("{} +0000", 1700000000 + sequence * 3600);
    let result = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=NUL",
        ])
        .env("GIT_AUTHOR_NAME", "Parcel Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Parcel Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .args(args)
        .output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(())
}
pub fn materialize(root: &Path) -> Result<()> {
    materialize_with_history(root, true)
}
pub(super) fn materialize_with_history(root: &Path, history: bool) -> Result<()> {
    if fs::read_dir(root)?.next().is_some() {
        return Err("materialization requires an empty directory".into());
    }
    let fixture = fixtures().join("mock-repo");
    if history {
        git(root, &["init", "-q", "-b", "main"], 0)?;
    }
    let mut groups = vec![Vec::new(); 37];
    let lanes = [
        "intake",
        "weighing",
        "sorting",
        "packing",
        "dispatch",
        "tracking",
        "customs",
        "delivery",
        "returns",
        "claims",
        "billing",
        "remittance",
    ];
    for p in files(&fixture)? {
        let name = p.to_string_lossy().replace('\\', "/");
        let dest = name.strip_suffix(".fixture").unwrap_or(&name);
        let body = fs::read_to_string(fixture.join(p))?;
        let key = dest.to_ascii_lowercase();
        let group = lanes
            .iter()
            .position(|l| key.contains(l))
            .map_or(0, |lane| {
                let phase = if key.starts_with("tests/")
                    || key.contains("_test")
                    || key.contains(".test.")
                    || key.contains("checks")
                    || key.contains("test_")
                {
                    2
                } else if key.starts_with("src/") || key.starts_with("config/") {
                    0
                } else {
                    1
                };
                1 + lane * 3 + phase
            });
        groups[group].push((dest.to_string(), body));
    }
    for (i, group) in groups.into_iter().enumerate() {
        for (p, body) in group {
            write(root, &p, &body)?;
        }
        if !history {
            continue;
        }
        git(root, &["add", "--", "."], i)?;
        let message = if i == 0 {
            "Initialize parcel clearinghouse".to_owned()
        } else {
            format!(
                "{}: {}",
                lanes[(i - 1) / 3],
                [
                    "set admission and pricing policy",
                    "connect scanner, console and carrier bridge",
                    "check admission and settlement boundaries"
                ][(i - 1) % 3]
            )
        };
        write(root, ".git/parcel-message", &message)?;
        git(root, &["commit", "-q", "-F", ".git/parcel-message"], i)?;
    }
    Ok(())
}
pub fn id(prefix: &str, key: &str) -> String {
    let bytes = Sha256::digest(format!("parcel-ledger-v1/{key}"));
    let mut n = u128::from_be_bytes(bytes[..16].try_into().unwrap());
    let mut out = [b'0'; 26];
    for ch in out.iter_mut().rev() {
        *ch = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(n & 31) as usize];
        n >>= 5;
    }
    format!("{prefix}_{}", std::str::from_utf8(&out).unwrap())
}
pub fn headings(kind: &str) -> &'static [&'static str] {
    match kind {
        "feature" => &["Purpose", "Behavior", "Stars"],
        "component" => &["Purpose", "Stars"],
        "contract" => &["Scope", "Contract", "Stars"],
        "test-suite" => &["Scope", "Tests"],
        "decision" => &["Context", "Decision", "Consequences"],
        "hub" => &["Purpose", "Map"],
        "glossary" => &["Terms"],
        _ => &[],
    }
}
pub fn note_title(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .replace('-', " ")
}
fn span(s: &str) -> String {
    let length = s.split(|c| c != '\u{60}').map(str::len).max().unwrap_or(0) + 1;
    let ticks = "\u{60}".repeat(length);
    format!("{ticks}{s}{ticks}")
}
impl Corpus {
    pub fn render(&self, root: &Path) -> Result<()> {
        self.render_formats(root, true, true)
    }
    pub fn render_formats(&self, root: &Path, legacy: bool, sky: bool) -> Result<()> {
        let mut notes: BTreeMap<&str, Vec<&Fact>> = BTreeMap::new();
        let lookup: BTreeMap<_, _> = self
            .facts
            .iter()
            .map(|f| (f.id.as_str(), f.constellation.path.as_str()))
            .collect();
        for fact in &self.facts {
            notes
                .entry(&fact.constellation.path)
                .or_default()
                .push(fact);
        }
        if sky {
            write(root, "observatory/sky.toml", "format_version = 1\n")?;
        }
        for (path, facts) in notes {
            let title = note_title(path);
            let kind = &facts[0].constellation.r#type;
            let cid = id("con", path);
            let mut obs = format!("---\ntype: {kind}\nid: {cid}\ntitle: {title}\n---\n");
            let mut sources = BTreeSet::new();
            let mut tests = BTreeSet::new();
            for f in &facts {
                if let Some(c) = &f.code {
                    if f.kind == "test" {
                        tests.insert(&c.path);
                    } else {
                        sources.insert(&c.path);
                    }
                }
            }
            let legacy_kind = match kind.as_str() {
                "component" => "architecture",
                "test-suite" => "test",
                other => other,
            };
            let mut old = format!(
                "---\ntype: {legacy_kind}\ntitle: {title}\nstatus: active\nsource-files: {}\ntest-files: {}\n---\n# {title}\n",
                serde_json::to_string(&sources)?,
                serde_json::to_string(&tests)?
            );
            for heading in headings(kind) {
                obs.push_str(&format!("\n## {heading}\n"));
                for f in facts.iter().filter(|f| f.constellation.heading == *heading) {
                    old.push_str(&format!("\n{}\n", f.text));
                    let sid = id("star", &f.id);
                    let mut callout = format!("> [!star] {sid} · {}\n> {}", f.kind, f.text);
                    if let Some(c) = &f.code {
                        let symbol = c
                            .symbol
                            .as_ref()
                            .map(|s| format!(" · symbol: {}", span(s)))
                            .unwrap_or_default();
                        callout.push_str(&format!(
                            "\n> coordinates: {}{symbol} · fp: {}",
                            span(&c.path),
                            span(&c.fp)
                        ));
                    }
                    let mut lines = BTreeSet::new();
                    for rel in &f.relations {
                        let target = lookup
                            .get(rel.target.as_str())
                            .ok_or("unknown relation target")?
                            .trim_end_matches(".md");
                        let ty = match rel.r#type.as_str() {
                            "contracts" | "tested-by" | "affects" => rel.r#type.as_str(),
                            _ => "link",
                        };
                        if lines.insert((ty, target)) {
                            callout.push_str(&format!("\n> {ty}:: [[{target}]]"));
                        }
                        old.push_str(&format!("\n[[knowledge/obsidian/{target}]]\n"));
                    }
                    if sky {
                        let base = hash(format!("{callout}\0{cid}\0{heading}").as_bytes());
                        write(
                            root,
                            &format!("observatory/.sky/stars/{sid}.md"),
                            &format!(
                                "live {sid} {cid} {base} agent parcel-{} parcel-{}\n",
                                f.feature, f.feature
                            ),
                        )?;
                        obs.push_str(&format!("\n{callout}\n"));
                    }
                }
            }
            if sky {
                write(root, &format!("observatory/{path}"), &obs)?;
            }
            if legacy {
                write(root, &format!("knowledge/obsidian/{path}"), &old)?;
            }
        }
        Ok(())
    }
    pub fn validate(&self, root: &Path) -> Result<()> {
        let mut markers = BTreeSet::new();
        let mut seen_notes = BTreeSet::new();
        let obs = root.join("observatory");
        let mut old_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut new_counts: BTreeMap<String, usize> = BTreeMap::new();
        for f in &self.facts {
            if !markers.insert(&f.text) {
                return Err("duplicate fact text".into());
            }
            let path = &f.constellation.path;
            let note = fs::read_to_string(obs.join(path))?;
            let legacy = fs::read_to_string(root.join("knowledge/obsidian").join(path))?;
            if note.matches(&f.text).count() != 1 || legacy.matches(&f.text).count() != 1 {
                return Err(f.id.clone().into());
            }
            let sections = headings(&f.constellation.r#type);
            if !sections.contains(&f.constellation.heading.as_str()) {
                return Err("invalid fact heading".into());
            }
            if seen_notes.insert(path) {
                for line in legacy.lines() {
                    *old_counts.entry(line.to_owned()).or_default() += 1;
                }
                for line in note.lines().filter_map(|line| line.strip_prefix("> ")) {
                    *new_counts.entry(line.to_owned()).or_default() += 1;
                }
                let yaml = note
                    .strip_prefix("---\n")
                    .and_then(|s| s.split_once("\n---\n"))
                    .ok_or("missing frontmatter")?
                    .0;
                let meta: serde_json::Value = serde_yaml::from_str(yaml)?;
                if meta["id"] != id("con", path)
                    || meta["type"] != f.constellation.r#type
                    || meta["title"] != note_title(path)
                    || note
                        .lines()
                        .filter_map(|l| l.strip_prefix("## "))
                        .collect::<Vec<_>>()
                        != sections
                {
                    return Err("invalid constellation header".into());
                }
            }
            if !(1..=3).contains(
                &f.text
                    .split_whitespace()
                    .filter(|w| w.ends_with(['.', '!', '?']))
                    .count(),
            ) || f.text.chars().count() > 400
            {
                return Err("invalid prose".into());
            }
            let sid = id("star", &f.id);
            let cid = id("con", path);
            let header = format!("> [!star] {sid} · {}\n> {}", f.kind, f.text);
            let (_, rest) = note.split_once(&header).ok_or("missing callout")?;
            let callout = format!(
                "{header}{}",
                rest.split_once("\n\n").map_or(rest.trim_end(), |(s, _)| s)
            );
            let present = callout
                .lines()
                .filter(|s| s.starts_with("> coordinates: "))
                .count();
            if present != usize::from(f.code.is_some())
                || (f.kind != "gotcha"
                    && f.code.is_none() != matches!(f.kind.as_str(), "decision" | "glossary"))
            {
                return Err("invalid coordinate cardinality".into());
            }
            let base = hash(format!("{callout}\0{cid}\0{}", f.constellation.heading).as_bytes());
            if fs::read_to_string(obs.join(format!(".sky/stars/{sid}.md")))?
                != format!(
                    "live {sid} {cid} {base} agent parcel-{} parcel-{}\n",
                    f.feature, f.feature
                )
            {
                return Err("invalid provenance".into());
            }
            for link in callout.lines().filter_map(|l| {
                l.split_once(":: [[")
                    .and_then(|(_, t)| t.strip_suffix("]]"))
            }) {
                if !obs.join(format!("{link}.md")).is_file() {
                    return Err("broken Line".into());
                }
            }
            if let Some(c) = &f.code {
                let body = fs::read_to_string(root.join(&c.path))?;
                if body.matches(&c.fp).count() != 1
                    || !body
                        .lines()
                        .nth(c.lines[0] - 1)
                        .is_some_and(|l| l.contains(&c.fp))
                    || !(8..=120).contains(&c.fp.chars().count())
                {
                    return Err(format!("invalid fingerprint on {}", f.id).into());
                }
            }
        }
        for f in &self.facts {
            if old_counts.get(&f.text) != Some(&1) || new_counts.get(&f.text) != Some(&1) {
                return Err(format!("fact occurs outside its assigned note: {}", f.id).into());
            }
        }
        if files(&obs.join(".sky/stars"))?.len() != self.facts.len() {
            return Err("orphan star metadata".into());
        }
        Ok(())
    }
}
