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
fn trigger_reports_kv_changes_and_new_backflow_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let html = dir.join("s.html");
    std::fs::write(&html, "<span data-kv=\"saved\"></span>").unwrap();
    let out = gui(dir, &["publish", "demo", "s", "--title", "S", "--alt", "s", "--html", html.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let kv = dir.join("demo/main/s/state/kv.json");
    std::fs::create_dir_all(kv.parent().unwrap()).unwrap();
    std::fs::write(&kv, r#"{"saved":1,"other":"x"}"#).unwrap();
    // History from before the first run is the baseline, never reported.
    assert!(gui(dir, &["backflow", "emit", "demo", "s", "--action", "old"]).status.success());

    let state = dir.join("snap.json");
    let st = state.to_str().unwrap();
    let events = |args: &[&str]| -> Vec<serde_json::Value> {
        let mut a = vec!["trigger", "--once", "--state", st];
        a.extend_from_slice(args);
        let out = gui(dir, &a);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        stdout(&out).lines().map(|l| serde_json::from_str(l).expect("JSON line")).collect()
    };
    assert!(events(&[]).is_empty(), "first run: silent baseline");

    std::fs::write(&kv, r#"{"saved":2,"other":"x"}"#).unwrap();
    let ws = gui_agent::Workspace::new(dir);
    let path = gui_schema::SurfacePath::new("demo", "main", "s").unwrap();
    ws.emit_stream(&path, "progress", serde_json::json!({"pct": 50})).unwrap();
    assert!(gui(dir, &["backflow", "emit", "demo", "s", "--action", "submit", "--value", r#"{"name":"Ada"}"#])
        .status
        .success());
    let evs = events(&["--with-stream", "--to", "running"]);
    assert_eq!(evs.len(), 3, "{evs:?}");
    let saved = evs.iter().find(|e| e["vars"]["kv_key"] == "saved").expect("saved changed");
    assert_eq!((saved["vars"]["kind"].clone(), saved["vars"]["old"].clone(), saved["vars"]["new"].clone()), ("kv".into(), 1.into(), 2.into()));
    assert_eq!(saved["to"], "running");
    assert!(evs.iter().any(|e| e["vars"]["kv_key"] == "stream.progress" && e["vars"]["new"]["pct"] == 50));
    let bf = evs.iter().find(|e| e["vars"]["kind"] == "backflow").expect("backflow event");
    assert_eq!((bf["vars"]["action"].clone(), bf["vars"]["value"]["name"].clone()), ("submit".into(), "Ada".into()));
    assert_eq!(bf["vars"]["surface"], "demo/main/s");

    // Nothing new: nothing repeats. Stream keys are off by default.
    ws.emit_stream(&path, "progress", serde_json::json!({"pct": 80})).unwrap();
    assert!(events(&[]).is_empty());

    // Filters: --action / --key, --no-backflow / --no-kv.
    for a in ["ping", "submit-2"] {
        assert!(gui(dir, &["backflow", "emit", "demo", "s", "--action", a]).status.success());
    }
    std::fs::write(&kv, r#"{"saved":3,"other":"y"}"#).unwrap();
    let evs = events(&["--action", "submit", "--key", "saved"]);
    let kinds: Vec<_> = evs.iter().map(|e| (e["vars"]["kind"].clone(), e["vars"]["action"].clone(), e["vars"]["kv_key"].clone())).collect();
    assert_eq!(evs.len(), 2, "{kinds:?}");
    assert!(gui(dir, &["backflow", "emit", "demo", "s", "--action", "submit"]).status.success());
    std::fs::write(&kv, r#"{"saved":4}"#).unwrap();
    assert!(events(&["--no-backflow"]).iter().all(|e| e["vars"]["kind"] == "kv"));
    // ...which kept the backflow watermark: only the one unseen event, not the history.
    let evs = events(&[]);
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(evs[0]["vars"]["action"], "submit");
}
