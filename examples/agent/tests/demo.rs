//! The example agent publishes a navigable 3-surface deck discoverable by the server.

#[test]
fn publishes_navigable_deck() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = gui_agent::Workspace::new(tmp.path());
    example_agent::publish_demo(&ws).unwrap();

    let idx = gui_server::Index::build(tmp.path());
    assert_eq!(idx.surface_count(), 3);

    let deck = idx.deck("demo", "main").unwrap();
    assert_eq!(deck.entry().unwrap().path.surface, "sales");
    let order: Vec<_> = deck
        .ordered_surfaces()
        .iter()
        .map(|s| s.path.surface.clone())
        .collect();
    assert_eq!(order, ["sales", "region-detail", "settings"]);

    // The sales surface links to the other two (navigable deck).
    let sales = idx
        .surface(&gui_schema::SurfacePath::new("demo", "main", "sales").unwrap())
        .unwrap();
    let targets: Vec<_> = sales.live().manifest.nav.iter().map(|n| n.to.as_str()).collect();
    assert!(targets.contains(&"region-detail") && targets.contains(&"settings"));
}
