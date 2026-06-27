//! Filesystem discovery: walk the artifacts directory into an in-memory [`Index`].
//!
//! The layout is a fixed depth — `{agent}/{deck}/{surface}/v/{version}/` — so we read it
//! level by level with `std::fs` (no recursive walker, no extra dependency). The newest
//! version of a surface is live; older versions are retained as history. Anything
//! malformed (bad slug, unparsable manifest, missing `index.html`) is skipped and recorded
//! in [`Index::warnings`] rather than aborting the scan.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use gui_schema::{is_valid_slug, AgentInfo, DeckManifest, Manifest, SurfacePath, Version};

/// One published version of a surface.
#[derive(Debug, Clone)]
pub struct SurfaceVersion {
    pub version: Version,
    pub manifest: Manifest,
    /// The `v/{version}` directory (holds `frame.json`, `index.html`, `assets/`).
    pub dir: PathBuf,
}

/// A surface with at least one valid version, sorted ascending (last = live).
#[derive(Debug, Clone)]
pub struct Surface {
    pub path: SurfacePath,
    pub versions: Vec<SurfaceVersion>,
}

impl Surface {
    /// The live (newest) version. A `Surface` always has ≥1 version.
    pub fn live(&self) -> &SurfaceVersion {
        self.versions.last().expect("surface always has >=1 version")
    }

    pub fn version(&self, v: Version) -> Option<&SurfaceVersion> {
        self.versions.iter().find(|sv| sv.version == v)
    }

    /// The earliest version id — used to order surfaces in a flat deck.
    pub fn first_seen(&self) -> Version {
        self.versions.first().expect("surface always has >=1 version").version
    }
}

/// A deck: a navigable set of interconnected surfaces under an agent.
#[derive(Debug, Clone)]
pub struct Deck {
    pub agent: String,
    pub slug: String,
    pub manifest: DeckManifest,
    pub surfaces: BTreeMap<String, Surface>,
}

impl Deck {
    /// Surfaces in navigation order: `deck.json` `order` first (de-duplicated, existing
    /// only), then the remainder by first-seen version, then slug.
    pub fn ordered_surfaces(&self) -> Vec<&Surface> {
        let mut out: Vec<&Surface> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for slug in &self.manifest.order {
            if let Some(s) = self.surfaces.get(slug) {
                if seen.insert(slug.as_str()) {
                    out.push(s);
                }
            }
        }
        let mut rest: Vec<&Surface> = self
            .surfaces
            .values()
            .filter(|s| !seen.contains(s.path.surface.as_str()))
            .collect();
        rest.sort_by(|a, b| {
            a.first_seen()
                .cmp(&b.first_seen())
                .then_with(|| a.path.surface.cmp(&b.path.surface))
        });
        out.extend(rest);
        out
    }

    /// The deck's entry surface: `deck.json` `entry` if it resolves, else the first in order.
    pub fn entry(&self) -> Option<&Surface> {
        if let Some(e) = &self.manifest.entry {
            if let Some(s) = self.surfaces.get(e) {
                return Some(s);
            }
        }
        self.ordered_surfaces().into_iter().next()
    }
}

/// An agent and its decks.
#[derive(Debug, Clone)]
pub struct Agent {
    pub slug: String,
    pub info: Option<AgentInfo>,
    pub decks: BTreeMap<String, Deck>,
}

/// The whole discovered tree.
#[derive(Debug, Clone)]
pub struct Index {
    pub root: PathBuf,
    pub agents: BTreeMap<String, Agent>,
    /// Non-fatal problems found while scanning (skipped artifacts).
    pub warnings: Vec<String>,
}

impl Index {
    /// Scan `root` into an index. Never fails: unreadable or malformed entries are skipped
    /// and recorded in [`Index::warnings`].
    pub fn build(root: &Path) -> Index {
        let mut idx = Index {
            root: root.to_path_buf(),
            agents: BTreeMap::new(),
            warnings: Vec::new(),
        };

        let agent_dirs = match read_subdirs(root) {
            Ok(d) => d,
            Err(e) => {
                idx.warnings
                    .push(format!("cannot read artifacts root {}: {e}", root.display()));
                return idx;
            }
        };

        for agent_slug in agent_dirs {
            if !is_valid_slug(&agent_slug) {
                idx.warnings
                    .push(format!("skip agent dir {agent_slug:?}: invalid slug"));
                continue;
            }
            let agent_dir = root.join(&agent_slug);
            let info = read_json_opt::<AgentInfo>(&agent_dir.join("agent.json"), &mut idx.warnings);

            let mut decks = BTreeMap::new();
            for deck_slug in read_subdirs(&agent_dir).unwrap_or_default() {
                if !is_valid_slug(&deck_slug) {
                    continue;
                }
                let deck_dir = agent_dir.join(&deck_slug);
                let manifest =
                    read_json_opt::<DeckManifest>(&deck_dir.join("deck.json"), &mut idx.warnings)
                        .unwrap_or_default();

                let mut surfaces = BTreeMap::new();
                for surface_slug in read_subdirs(&deck_dir).unwrap_or_default() {
                    let path = match SurfacePath::new(&agent_slug, &deck_slug, &surface_slug) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    if let Some(s) = build_surface(root, path, &mut idx.warnings) {
                        surfaces.insert(surface_slug, s);
                    }
                }

                if !surfaces.is_empty() {
                    decks.insert(
                        deck_slug.clone(),
                        Deck {
                            agent: agent_slug.clone(),
                            slug: deck_slug,
                            manifest,
                            surfaces,
                        },
                    );
                }
            }

            if !decks.is_empty() {
                idx.agents.insert(
                    agent_slug.clone(),
                    Agent {
                        slug: agent_slug,
                        info,
                        decks,
                    },
                );
            }
        }

        idx
    }

    pub fn agent(&self, slug: &str) -> Option<&Agent> {
        self.agents.get(slug)
    }

    pub fn deck(&self, agent: &str, deck: &str) -> Option<&Deck> {
        self.agents.get(agent)?.decks.get(deck)
    }

    pub fn surface(&self, p: &SurfacePath) -> Option<&Surface> {
        self.deck(&p.agent, &p.deck)?.surfaces.get(&p.surface)
    }

    /// Total number of surfaces across all agents/decks.
    pub fn surface_count(&self) -> usize {
        self.agents
            .values()
            .flat_map(|a| a.decks.values())
            .map(|d| d.surfaces.len())
            .sum()
    }
}

fn build_surface(root: &Path, path: SurfacePath, warnings: &mut Vec<String>) -> Option<Surface> {
    let vdir = path.dir(root).join("v");
    let mut versions = Vec::new();

    for vname in read_subdirs(&vdir).unwrap_or_default() {
        let version = match Version::parse(&vname) {
            Ok(v) => v,
            Err(_) => {
                warnings.push(format!("skip {} version {vname:?}: not a version id", path.id()));
                continue;
            }
        };
        let dir = vdir.join(&vname);
        let manifest: Manifest = match read_json(&dir.join("frame.json")) {
            Ok(m) => m,
            Err(e) => {
                warnings.push(format!("skip {} v{vname}: bad frame.json: {e}", path.id()));
                continue;
            }
        };
        if !dir.join("index.html").is_file() {
            warnings.push(format!("skip {} v{vname}: missing index.html", path.id()));
            continue;
        }
        versions.push(SurfaceVersion {
            version,
            manifest,
            dir,
        });
    }

    if versions.is_empty() {
        return None;
    }
    versions.sort_by_key(|sv| sv.version);
    Some(Surface { path, versions })
}

/// Immediate subdirectory names of `dir` (non-recursive).
fn read_subdirs(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
    }
    Ok(out)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn read_json_opt<T: serde::de::DeserializeOwned>(
    path: &Path,
    warnings: &mut Vec<String>,
) -> Option<T> {
    if !path.exists() {
        return None;
    }
    match read_json(path) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("ignoring {}: {e}", path.display()));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: PathBuf, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn manifest_json(title: &str) -> String {
        format!(
            r#"{{"schema_version":"generative-ui/surface.v1","title":"{title}","alt_text":"a"}}"#
        )
    }

    /// Build a fixture tree and assert live resolution, history, deck order, and skips.
    #[test]
    fn discovers_versions_decks_and_skips_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // demo/main/sales with two versions (200 newer than 100).
        for v in ["100", "200"] {
            let d = root.join("demo/main/sales/v").join(v);
            write(d.join("frame.json"), &manifest_json("Sales"));
            write(d.join("index.html"), "<ui-text>hi</ui-text>");
        }
        // demo/main/settings, one version, earlier than sales.
        let d = root.join("demo/main/settings/v/50");
        write(d.join("frame.json"), &manifest_json("Settings"));
        write(d.join("index.html"), "x");
        // deck.json orders settings before sales, entry = settings.
        write(
            root.join("demo/main/deck.json"),
            r#"{"entry":"settings","order":["settings","sales"]}"#,
        );

        // A broken surface: invalid JSON manifest -> skipped with a warning.
        write(
            root.join("demo/main/broken/v/10/frame.json"),
            "{ not json",
        );
        write(root.join("demo/main/broken/v/10/index.html"), "x");
        // A version dir that isn't a version id -> skipped.
        write(
            root.join("demo/main/sales/v/notaversion/frame.json"),
            &manifest_json("x"),
        );

        let idx = Index::build(root);

        assert_eq!(idx.agents.len(), 1);
        assert_eq!(idx.surface_count(), 2, "sales + settings, broken skipped");

        let sales = idx
            .surface(&SurfacePath::new("demo", "main", "sales").unwrap())
            .unwrap();
        assert_eq!(sales.live().version, Version(200), "newest version is live");
        assert_eq!(sales.versions.len(), 2, "history retained");

        let deck = idx.deck("demo", "main").unwrap();
        let order: Vec<&str> = deck
            .ordered_surfaces()
            .iter()
            .map(|s| s.path.surface.as_str())
            .collect();
        assert_eq!(order, ["settings", "sales"], "deck.json order honored");
        assert_eq!(deck.entry().unwrap().path.surface, "settings");

        assert!(
            idx.warnings.iter().any(|w| w.contains("broken")),
            "malformed surface recorded: {:?}",
            idx.warnings
        );
    }

    #[test]
    fn flat_deck_orders_by_first_seen() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // No deck.json -> order by first-seen version: b(1) before a(2).
        let a = root.join("ag/main/a/v/2");
        write(a.join("frame.json"), &manifest_json("A"));
        write(a.join("index.html"), "x");
        let b = root.join("ag/main/b/v/1");
        write(b.join("frame.json"), &manifest_json("B"));
        write(b.join("index.html"), "x");

        let idx = Index::build(root);
        let deck = idx.deck("ag", "main").unwrap();
        let order: Vec<&str> = deck
            .ordered_surfaces()
            .iter()
            .map(|s| s.path.surface.as_str())
            .collect();
        assert_eq!(order, ["b", "a"]);
    }
}
