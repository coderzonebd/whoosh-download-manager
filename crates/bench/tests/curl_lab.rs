//! End to end: the `vayu-bench` binary runs curl against an in-process
//! labserver, verifies every file, writes the raw data and renders a report.
//! Skipped (with a message) where curl is not installed.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_vayu-bench")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vayu-bench-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn have_curl() -> bool {
    Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run(dir: &Path, label: &str, extra: &[&str]) -> Vec<Value> {
    let out = Command::new(exe())
        .args([
            "run",
            "--clients",
            "curl",
            "--seed",
            "42",
            "--quiet",
            "--label",
            label,
        ])
        .arg("--out")
        .arg(dir.join("data"))
        .arg("--work-dir")
        .arg(dir.join("work"))
        .args(extra)
        // Keep fetched tools out of the picture.
        .env("VAYU_BENCH_CACHE", dir.join("cache"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vayu-bench run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let jsonl = std::fs::read_to_string(dir.join("data").join(label).join("runs.jsonl")).unwrap();
    jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn curl_against_labserver() {
    if !have_curl() {
        eprintln!("curl not installed; skipping");
        return;
    }
    let dir = scratch("curl");

    // 1. Clean downloads: all correct, one connection, every metric present.
    let runs = run(
        &dir,
        "clean",
        &[
            "--scenarios",
            "clean",
            "--files",
            "1m.bin,10m.bin",
            "--runs",
            "3",
            "--force-runs",
        ],
    );
    assert_eq!(runs.len(), 6);
    for r in &runs {
        assert_eq!(r["client"], "curl");
        assert_eq!(r["outcome"], "ok", "{r}");
        assert_eq!(r["sha256"], r["expected_sha256"]);
        assert_eq!(r["exit_code"], 0);
        assert_eq!(r["peak_connections"], 1, "curl uses one connection");
        assert_eq!(r["status"]["200"], 1);
        let wall = r["wall_s"].as_f64().unwrap();
        let last = r["last_byte_s"].as_f64().unwrap();
        let ttfb = r["ttfb_s"].as_f64().unwrap();
        assert!(ttfb > 0.0 && ttfb <= last && last <= wall, "{r}");
        assert!(r["throughput_mib_s"].as_f64().unwrap() > 0.0);
        if cfg!(unix) {
            assert!(r["max_rss_mib"].as_f64().unwrap() > 0.5, "{r}");
            assert!(r["cpu_user_s"].as_f64().is_some());
        }
        if cfg!(target_os = "linux") {
            // The idle-machine guard sampled the machine before the run.
            assert!(r["busy_cpus"].as_f64().is_some(), "{r}");
            assert!(r["machine_busy"].is_boolean());
        }
        assert!(r.get("resident").is_none(), "curl has no resident helper");
    }
    let reps: Vec<u64> = runs.iter().map(|r| r["rep"].as_u64().unwrap()).collect();
    assert_ne!(reps, [0, 1, 2, 0, 1, 2], "order is shuffled");

    // 2. Names from Content-Disposition, and the traversal kept inside.
    let runs = run(
        &dir,
        "names",
        &[
            "--scenarios",
            "bangla-filename",
            "--files",
            "bangla.bin,traversal.bin",
            "--runs",
            "1",
            "--force-runs",
        ],
    );
    assert_eq!(runs.len(), 2);
    for r in &runs {
        assert_eq!(r["outcome"], "ok", "{r}");
        assert_ne!(r["name_check"], "escaped");
        if r["file"] == "traversal.bin" {
            assert_eq!(r["output_name"], "passwd");
        }
    }

    // 3. Timeouts: 10 MiB at 2 MB/s needs ~5 s; a 2 s limit kills curl, and
    // after two timeouts the third run is skipped.
    let runs = run(
        &dir,
        "timeout",
        &[
            "--scenarios",
            "per-conn-throttle",
            "--files",
            "10m.bin",
            "--runs",
            "3",
            "--force-runs",
            "--timeout-scale",
            "0.03",
        ],
    );
    let outcomes: Vec<&str> = runs
        .iter()
        .map(|r| r["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["timeout", "timeout", "skipped"], "{runs:?}");
    assert_eq!(runs[0]["timed_out"], true);
    assert!(
        runs[0]["wall_s"].as_f64().unwrap() < 10.0,
        "killed, not waited out"
    );

    // 4. The report renders from the raw data alone.
    let report = dir.join("report.md");
    let out = Command::new(exe())
        .args(["report"])
        .arg(dir.join("data").join("clean"))
        .arg("-o")
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = std::fs::read_to_string(&report).unwrap();
    assert!(md.contains("# vayu-bench report: `clean`"));
    assert!(md.contains("### clean — `10m.bin` (10 MiB)"));
    assert!(md.contains("| curl | 3/3 |"));
    assert!(md.contains("Every run produced a byte-exact file."));

    let _ = std::fs::remove_dir_all(&dir);
}
