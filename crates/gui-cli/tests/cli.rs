//! Stage 9: drive the built `gui` binary against a temp artifacts dir.

use std::path::Path;
use std::process::{Command, Output};

fn gui(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gui"))
        .arg("--artifacts")
        .arg(dir)
        .args(args)
        .output()
        .expect("run gui")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn publish_index_deck_backflow_validate() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // publish from a component-tree file
    let comp = dir.join("comp.json");
    std::fs::write(
        &comp,
        r#"[{"type":"heading","text":"Sales"},{"type":"button","label":"Refresh","action":"refresh"}]"#,
    )
    .unwrap();
    let out = gui(
        dir,
        &["publish", "demo", "sales", "--title", "Sales", "--alt", "alt", "--components", comp.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("published demo/main/sales"));

    // index lists it
    let out = gui(dir, &["index"]);
    let s = stdout(&out);
    assert!(s.contains("demo") && s.contains("sales") && s.contains("1 surface(s)"), "{s}");

    // deck graph as JSON
    let out = gui(dir, &["deck", "demo", "main"]);
    assert!(stdout(&out).contains("\"sales\""));

    // surface manifest
    let out = gui(dir, &["surface", "demo", "sales"]);
    assert!(stdout(&out).contains("\"title\": \"Sales\""));

    // emit backflow
    let out = gui(dir, &["backflow", "emit", "demo", "sales", "--action", "ping", "--value", "{\"a\":1}"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("wrote"));
    // the event landed
    let events = std::fs::read_dir(dir.join("demo/main/sales/events")).unwrap().count();
    assert_eq!(events, 1);

    // validate: clean tree exits 0
    let out = gui(dir, &["validate"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("0 error(s)"));

    // a malformed manifest fails validation with a non-zero exit
    let bad = dir.join("demo/main/sales/v/1/frame.json");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, "{ not json").unwrap();
    let out = gui(dir, &["validate"]);
    assert!(!out.status.success(), "validate should fail on bad manifest");
    assert!(stdout(&out).contains("ERROR"));
}

#[test]
fn kv_trigger_reports_each_changed_key_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let html = dir.join("s.html");
    std::fs::write(&html, "<span data-kv=\"saved\"></span>").unwrap();
    let out = gui(dir, &["publish", "demo", "s", "--title", "S", "--alt", "s", "--html", html.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let kv = dir.join("demo/main/s/state/kv.json");
    std::fs::create_dir_all(kv.parent().unwrap()).unwrap();
    std::fs::write(&kv, r#"{"saved":1,"other":"x"}"#).unwrap();

    let state = dir.join("snap.json");
    let run = || gui(dir, &["kv-trigger", "--once", "--with-stream", "--state", state.to_str().unwrap(), "--to", "running"]);
    // First run: silent baseline.
    assert_eq!(stdout(&run()), "");

    std::fs::write(&kv, r#"{"saved":2,"other":"x"}"#).unwrap();
    let ws = gui_agent::Workspace::new(dir);
    let path = gui_schema::SurfacePath::new("demo", "main", "s").unwrap();
    ws.emit_stream(&path, "progress", serde_json::json!({"pct": 50})).unwrap();
    let out = run();
    let events: Vec<serde_json::Value> =
        stdout(&out).lines().map(|l| serde_json::from_str(l).expect("JSON line")).collect();
    assert_eq!(events.len(), 2, "{events:?}");
    let saved = events.iter().find(|e| e["vars"]["kv_key"] == "saved").expect("saved changed");
    assert_eq!((saved["vars"]["old"].clone(), saved["vars"]["new"].clone()), (1.into(), 2.into()));
    assert_eq!(saved["to"], "running");
    assert_eq!(saved["vars"]["surface"], "demo/main/s");
    assert!(events.iter().any(|e| e["vars"]["kv_key"] == "stream.progress" && e["vars"]["new"]["pct"] == 50));

    // Nothing changed since: nothing repeats.
    assert_eq!(stdout(&run()), "");
    // Without --with-stream, stream keys are ignored.
    ws.emit_stream(&path, "progress", serde_json::json!({"pct": 80})).unwrap();
    assert_eq!(stdout(&gui(dir, &["kv-trigger", "--once", "--state", state.to_str().unwrap()])), "");
    // --key filters.
    std::fs::write(&kv, r#"{"saved":3,"other":"y"}"#).unwrap();
    let out = gui(dir, &["kv-trigger", "--once", "--state", state.to_str().unwrap(), "--key", "saved"]);
    assert_eq!(stdout(&out).lines().count(), 1, "{}", stdout(&out));
}
