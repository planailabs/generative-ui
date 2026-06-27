//! Example agent: builds a small navigable deck programmatically via `gui-agent`.

use anyhow::Result;
use gui_agent::{SurfaceBuilder, Workspace};
use gui_schema::{Component, DeckManifest, SelectOption, SurfacePath};
use serde_json::json;

pub const AGENT: &str = "demo";
pub const DECK: &str = "main";

/// Publish the three-surface demo deck (sales → region-detail → settings) and wire it.
pub fn publish_demo(ws: &Workspace) -> Result<()> {
    let sales = SurfacePath::new(AGENT, DECK, "sales")?;
    ws.publish(
        &sales,
        &SurfaceBuilder::new("Sales", "Sales overview with a refresh action")
            .heading("Sales")
            .component(Component::Text {
                text: "Revenue is up 12% this week.".into(),
            })
            .button("Refresh", "refresh", json!(null))
            .nav("region-detail", Some("Regions"))
            .nav("settings", Some("Settings"))
            .component(Component::DeckNav),
    )?;

    let region = SurfacePath::new(AGENT, DECK, "region-detail")?;
    ws.publish(
        &region,
        &SurfaceBuilder::new("Regions", "Per-region breakdown")
            .heading("Regions")
            .component(Component::Select {
                name: "region".into(),
                label: Some("Region".into()),
                options: vec![
                    SelectOption { value: "emea".into(), label: Some("EMEA".into()) },
                    SelectOption { value: "amer".into(), label: Some("Americas".into()) },
                    SelectOption { value: "apac".into(), label: Some("APAC".into()) },
                ],
            })
            .button("Apply", "apply", json!(null))
            .nav("sales", Some("Back to Sales"))
            .component(Component::DeckNav),
    )?;

    let settings = SurfacePath::new(AGENT, DECK, "settings")?;
    ws.publish(
        &settings,
        &SurfaceBuilder::new("Settings", "Dashboard settings form")
            .heading("Settings")
            .component(Component::Form {
                action: "save".into(),
                children: vec![
                    Component::Input {
                        name: "threshold".into(),
                        label: Some("Alert threshold".into()),
                        placeholder: Some("1000".into()),
                        input_type: Some("number".into()),
                    },
                    Component::Checkbox {
                        name: "email".into(),
                        label: "Email me alerts".into(),
                        checked: true,
                    },
                ],
            })
            .action("save")
            .nav("sales", Some("Back to Sales"))
            .component(Component::DeckNav),
    )?;

    ws.define_deck(
        AGENT,
        DECK,
        &DeckManifest {
            name: Some("Demo dashboard".into()),
            entry: Some("sales".into()),
            order: vec!["sales".into(), "region-detail".into(), "settings".into()],
        },
    )?;
    Ok(())
}
