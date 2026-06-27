//! JSON API response types and builders over the discovered [`Index`].
//!
//! These are the machine-readable views: the full index, the chronological activity feed,
//! the deck graph (used for both navigation and the inspector), and a surface's version
//! history.

use serde::Serialize;

use crate::discovery::{Deck, Index, Surface};

/// Canonical live URL for a surface: `/{agent}/{deck}/{surface}/`.
fn surface_url(s: &Surface) -> String {
    s.path.url()
}

/// Versioned URL: `/{agent}/{deck}/{surface}/v/{version}/`.
fn version_url(s: &Surface, version: &str) -> String {
    format!("{}v/{}/", s.path.url(), version)
}

#[derive(Serialize)]
pub struct IndexResponse {
    pub agents: Vec<AgentSummary>,
    pub surface_count: usize,
}

#[derive(Serialize)]
pub struct AgentSummary {
    pub slug: String,
    pub name: Option<String>,
    pub decks: Vec<DeckSummary>,
}

#[derive(Serialize)]
pub struct DeckSummary {
    pub slug: String,
    pub name: Option<String>,
    pub entry: Option<String>,
    pub surfaces: Vec<SurfaceSummary>,
}

#[derive(Serialize)]
pub struct SurfaceSummary {
    pub slug: String,
    pub id: String,
    pub title: String,
    pub url: String,
    pub live_version: String,
    pub versions: usize,
}

fn surface_summary(s: &Surface) -> SurfaceSummary {
    let live = s.live();
    SurfaceSummary {
        slug: s.path.surface.clone(),
        id: s.path.id(),
        title: live.manifest.title.clone(),
        url: surface_url(s),
        live_version: live.version.to_string(),
        versions: s.versions.len(),
    }
}

pub fn index_response(idx: &Index) -> IndexResponse {
    let agents = idx
        .agents
        .values()
        .map(|a| AgentSummary {
            slug: a.slug.clone(),
            name: a.info.as_ref().and_then(|i| i.name.clone()),
            decks: a
                .decks
                .values()
                .map(|d| DeckSummary {
                    slug: d.slug.clone(),
                    name: d.manifest.name.clone(),
                    entry: d.entry().map(|s| s.path.surface.clone()),
                    surfaces: d.ordered_surfaces().into_iter().map(surface_summary).collect(),
                })
                .collect(),
        })
        .collect();
    IndexResponse {
        agents,
        surface_count: idx.surface_count(),
    }
}

#[derive(Serialize)]
pub struct ActivityResponse {
    pub publishes: Vec<ActivityItem>,
}

#[derive(Serialize)]
pub struct ActivityItem {
    pub surface: String,
    pub version: String,
    pub title: String,
    pub url: String,
    pub created_at: Option<String>,
}

/// All published versions across all agents, newest first.
pub fn activity_response(idx: &Index) -> ActivityResponse {
    let mut publishes: Vec<(u64, ActivityItem)> = Vec::new();
    for agent in idx.agents.values() {
        for deck in agent.decks.values() {
            for surface in deck.surfaces.values() {
                for sv in &surface.versions {
                    publishes.push((
                        sv.version.0,
                        ActivityItem {
                            surface: surface.path.id(),
                            version: sv.version.to_string(),
                            title: sv.manifest.title.clone(),
                            url: version_url(surface, &sv.version.to_string()),
                            created_at: sv.manifest.created_at.clone(),
                        },
                    ));
                }
            }
        }
    }
    publishes.sort_by_key(|p| std::cmp::Reverse(p.0));
    ActivityResponse {
        publishes: publishes.into_iter().map(|(_, item)| item).collect(),
    }
}

#[derive(Serialize)]
pub struct DeckGraph {
    pub agent: String,
    pub deck: String,
    pub name: Option<String>,
    pub entry: Option<String>,
    /// Surface slugs in navigation order.
    pub order: Vec<String>,
    pub surfaces: Vec<DeckNode>,
}

#[derive(Serialize)]
pub struct DeckNode {
    pub slug: String,
    pub title: String,
    pub url: String,
    pub nav: Vec<NavEdgeOut>,
}

#[derive(Serialize)]
pub struct NavEdgeOut {
    pub to: String,
    pub label: Option<String>,
    /// Resolved URL if the target surface exists in this deck.
    pub url: Option<String>,
}

/// The deck graph: ordered surfaces plus each surface's resolved nav edges. Drives both
/// in-browser navigation and the inspector view.
pub fn deck_graph(deck: &Deck) -> DeckGraph {
    let ordered = deck.ordered_surfaces();
    let order: Vec<String> = ordered.iter().map(|s| s.path.surface.clone()).collect();

    let surfaces = ordered
        .iter()
        .map(|s| {
            let nav = s
                .live()
                .manifest
                .nav
                .iter()
                .map(|edge| NavEdgeOut {
                    to: edge.to.clone(),
                    label: edge.label.clone(),
                    url: deck.surfaces.get(&edge.to).map(surface_url),
                })
                .collect();
            DeckNode {
                slug: s.path.surface.clone(),
                title: s.live().manifest.title.clone(),
                url: surface_url(s),
                nav,
            }
        })
        .collect();

    DeckGraph {
        agent: deck.agent.clone(),
        deck: deck.slug.clone(),
        name: deck.manifest.name.clone(),
        entry: deck.entry().map(|s| s.path.surface.clone()),
        order,
        surfaces,
    }
}

#[derive(Serialize)]
pub struct VersionsResponse {
    pub surface: String,
    pub live: String,
    pub versions: Vec<VersionInfo>,
}

#[derive(Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub title: String,
    pub created_at: Option<String>,
    pub url: String,
}

/// A surface's version history, newest first.
pub fn versions_response(surface: &Surface) -> VersionsResponse {
    let mut versions: Vec<VersionInfo> = surface
        .versions
        .iter()
        .rev()
        .map(|sv| VersionInfo {
            version: sv.version.to_string(),
            title: sv.manifest.title.clone(),
            created_at: sv.manifest.created_at.clone(),
            url: version_url(surface, &sv.version.to_string()),
        })
        .collect();
    versions.shrink_to_fit();
    VersionsResponse {
        surface: surface.path.id(),
        live: surface.live().version.to_string(),
        versions,
    }
}
