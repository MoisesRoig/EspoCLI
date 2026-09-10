//! Read-only and dry-run checks against a real instance.
//! Ignored by default; run with: ESPO_TEST_URL=... ESPO_TEST_API_KEY=... cargo test -- --ignored

use std::process::Command;

struct Live {
    url: String,
    key: String,
    home: std::path::PathBuf,
}

impl Live {
    fn from_env() -> Option<Self> {
        let url = std::env::var("ESPO_TEST_URL").ok().filter(|s| !s.is_empty())?;
        let key = std::env::var("ESPO_TEST_API_KEY").ok().filter(|s| !s.is_empty())?;
        let home = std::env::temp_dir().join(format!("espocli-live-{}", std::process::id()));
        std::fs::create_dir_all(&home).ok()?;
        Some(Self { url, key, home })
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_espo"))
            .args(args)
            .env("ESPO_URL", &self.url)
            .env("ESPO_API_KEY", &self.key)
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .output()
            .expect("running espo");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
#[ignore = "needs ESPO_TEST_URL and ESPO_TEST_API_KEY"]
fn read_only_surface_works() {
    let Some(live) = Live::from_env() else {
        panic!("set ESPO_TEST_URL and ESPO_TEST_API_KEY");
    };

    let (ok, out, err) = live.run(&["status", "--no-header"]);
    assert!(ok, "status failed: {err}");
    for key in ["profile", "url", "version", "user", "cache"] {
        assert!(out.lines().any(|l| l.starts_with(&format!("{key}\t"))), "status has no {key}: {out}");
    }

    let (ok, out, _) = live.run(&["auth", "status"]);
    assert!(ok, "auth status failed: {out}");

    let (ok, out, err) = live.run(&["entities", "-o", "--no-header"]);
    assert!(ok, "entities failed: {err}");
    assert!(out.lines().count() > 10, "expected many entities, got {out}");

    let (ok, out, err) = live.run(&["schema", "Lead", "-f", "status", "--no-header"]);
    assert!(ok, "schema failed: {err}");
    assert!(out.starts_with("status\tenum\t"), "unexpected schema row: {out}");

    // Case-insensitive entity resolution and TSV projection to exactly the selected columns.
    let (ok, out, err) = live.run(&["list", "lead", "-n", "2", "-s", "id,status", "--no-header"]);
    assert!(ok, "list failed: {err}");
    for line in out.lines() {
        assert_eq!(line.split('\t').count(), 2, "row is not 2 columns: {line}");
    }
    assert!(err.contains('/'), "expected a count on stderr, got {err}");

    let (ok, out, err) = live.run(&["--dry-run", "report", "abc123", "-n", "3"]);
    assert!(ok, "dry-run report failed: {err}");
    assert!(out.contains("Report/action/runList?id=abc123&maxSize=3"), "unexpected report request: {out}");
}

#[test]
#[ignore = "needs ESPO_TEST_URL and ESPO_TEST_API_KEY"]
fn writes_are_only_described_in_dry_run() {
    let Some(live) = Live::from_env() else {
        panic!("set ESPO_TEST_URL and ESPO_TEST_API_KEY");
    };

    let (ok, out, err) = live.run(&["--dry-run", "create", "Lead", "lastName=DryRun", "doNotCall=true"]);
    assert!(ok, "dry-run create failed: {err}");
    assert!(out.contains("POST "), "no method in output: {out}");
    assert!(out.contains("\"doNotCall\":true"), "bool was not coerced: {out}");

    let (ok, _, err) = live.run(&["raw", "POST", "Contact", "-H", "no-colon", "--data", "{}"]);
    assert!(!ok, "a header without a colon must fail");
    assert!(err.contains("Name:value"), "unhelpful header message: {err}");

    let (ok, _, err) = live.run(&["delete", "Lead", "whatever"]);
    assert!(!ok, "delete without --yes must fail");
    assert!(err.contains("--yes"), "unhelpful guard message: {err}");
}
