//! Surface templates: minijinja HTML with slots, rendered into a surface body.
//!
//! Built-ins ship embedded; an optional templates dir holds model-authored ones
//! (`{dir}/{name}.html`; a file shadows a built-in of the same name). Templates can
//! `{% include %}`/`{% extends %}` each other by name, so skeletons compose.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use gui_schema::{atomic_write, is_valid_slug};
use minijinja::{AutoEscape, Environment, Error, ErrorKind};
use serde::Serialize;

const BUILTINS: &[(&str, &str)] = &[
    ("skeleton", include_str!("../templates/skeleton.html")),
    ("form", include_str!("../templates/form.html")),
    ("list", include_str!("../templates/list.html")),
    ("dashboard", include_str!("../templates/dashboard.html")),
    ("nav", include_str!("../templates/nav.html")),
];

/// Per-render instruction budget, so a model-authored template can't loop forever.
const FUEL: u64 = 1_000_000;

#[derive(Debug, Serialize)]
pub struct TemplateInfo {
    pub name: String,
    /// `builtin` or `custom` (from the templates dir).
    pub source: &'static str,
    /// The template's leading `{# ... #}` comment: what it is and its slots.
    pub description: String,
}

#[derive(Clone, Debug, Default)]
pub struct Templates {
    dir: Option<PathBuf>,
}

fn builtin(name: &str) -> Option<&'static str> {
    BUILTINS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

fn description(source: &str) -> String {
    source
        .trim_start()
        .strip_prefix("{#")
        .and_then(|s| s.split_once("#}"))
        .map(|(c, _)| c.trim().to_string())
        .unwrap_or_default()
}

impl Templates {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{name}.html")))
    }

    /// Source of `name` and whether it is `builtin` or `custom`.
    pub fn read(&self, name: &str) -> Result<Option<(String, &'static str)>> {
        if !is_valid_slug(name) {
            return Ok(None);
        }
        if let Some(path) = self.file(name) {
            match std::fs::read_to_string(&path) {
                Ok(s) => return Ok(Some((s, "custom"))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            }
        }
        Ok(builtin(name).map(|s| (s.to_string(), "builtin")))
    }

    fn env(&self) -> Environment<'static> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        env.set_fuel(Some(FUEL));
        let this = self.clone();
        env.set_loader(move |name| {
            this.read(name)
                .map(|t| t.map(|(s, _)| s))
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("{e:#}")))
        });
        env
    }

    /// Render template `name` with `ctx` into a surface body.
    pub fn render(&self, name: &str, ctx: &serde_json::Value) -> Result<String> {
        let env = self.env();
        let tmpl = env.get_template(name).map_err(|e| anyhow::anyhow!("template {name:?}: {e:#}"))?;
        tmpl.render(minijinja::value::Serde(ctx)).map_err(|e| anyhow::anyhow!("render {name:?}: {e:#}"))
    }

    /// Built-ins plus the templates dir's `*.html`, custom shadowing built-in.
    pub fn list(&self) -> Result<Vec<TemplateInfo>> {
        let mut names: Vec<String> = BUILTINS.iter().map(|(n, _)| n.to_string()).collect();
        if let Some(dir) = &self.dir {
            match std::fs::read_dir(dir) {
                Ok(rd) => {
                    for e in rd {
                        let p = e?.path();
                        if p.extension().is_some_and(|x| x == "html")
                            && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                            && is_valid_slug(stem)
                            && !names.iter().any(|n| n == stem)
                        {
                            names.push(stem.to_string());
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("list {}", dir.display())),
            }
        }
        names.sort();
        let mut out = Vec::new();
        for name in names {
            if let Some((src, source)) = self.read(&name)? {
                out.push(TemplateInfo { description: description(&src), name, source });
            }
        }
        Ok(out)
    }

    /// Validate (it must compile) and atomically write a custom template.
    pub fn save(&self, name: &str, source: &str) -> Result<PathBuf> {
        if !is_valid_slug(name) {
            bail!("invalid template name {name:?} (lowercase slug: a-z, 0-9, -)");
        }
        let Some(path) = self.file(name) else {
            bail!("no templates dir configured (run gui-mcp with --templates)");
        };
        self.env()
            .template_from_named_str(name, source)
            .map_err(|e| anyhow::anyhow!("template {name:?} doesn't compile: {e:#}"))?;
        atomic_write(&path, source.as_bytes()).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}
