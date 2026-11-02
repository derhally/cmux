//! SCRATCH measurement (never merge; agent split-burst): 4 splits sent at
//! once, as the app sends Cmd-D (legacy `split` with client-minted ids),
//! repeated over fresh workspaces. Prints each round's answer times.

use super::super::*;
use std::time::Instant;

const ROUNDS: usize = 12;
const BURST: usize = 4;

fn key(prefix: &str, n: u64) -> String {
    format!("{prefix}{:032x}", 0x5b00_0000_0000_0000_0000_0000_0000_0000u128 + u128::from(n))
}

fn terminal_key(n: u64) -> String {
    format!("5b000000000040008000{n:012x}")
}

fn pane_of_workspace(harness: &RecoveryHarness, name: &str) -> u64 {
    let tree = request(&harness.socket, serde_json::json!({"id": 9_100, "cmd": "list-workspaces"}));
    tree["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"].as_str() == Some(name))
        .and_then(|w| w["screens"][0]["panes"][0]["id"].as_u64())
        .unwrap_or_else(|| panic!("no pane for {name}: {tree}"))
}

#[test]
fn scratch_split_burst_measure() {
    let mut harness = RecoveryHarness::start_unstarted("split-burst");
    let spans_path = harness.dir.join("spans.jsonl");
    harness.extra_env.push(("CMUX_TUI_DEBUG_SPANS".into(), spans_path.display().to_string()));
    harness.restart();
    let mut rounds = Vec::new();
    let mut all = Vec::new();
    let mut spreads = Vec::new();
    for round in 0..ROUNDS {
        let name = format!("burst-{round}");
        run_cat_workspace(&harness.socket, 100 + round, &name);
        let pane = pane_of_workspace(&harness, &name);
        std::thread::sleep(Duration::from_millis(300));
        let start = Instant::now();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(BURST));
        let threads = (0..BURST)
            .map(|n| {
                let socket = harness.socket.clone();
                let barrier = barrier.clone();
                let k = (round * BURST + n) as u64;
                std::thread::spawn(move || {
                    barrier.wait();
                    let sent = Instant::now();
                    let answer = request(
                        &socket,
                        serde_json::json!({"id": format!("s{k}"), "cmd": "split", "pane": pane,
                            "dir": "down", "cols": 80, "rows": 12,
                            "pane_id": key("pane_", k), "tab_id": key("tab_", k),
                            "terminal_id": terminal_key(k)}),
                    );
                    assert_ne!(answer["ok"], serde_json::json!(false), "{answer}");
                    (sent.elapsed().as_micros() as f64 / 1000.0, Instant::now())
                })
            })
            .collect::<Vec<_>>();
        let results = threads.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>();
        let mut latencies = results.iter().map(|(ms, _)| *ms).collect::<Vec<_>>();
        latencies.sort_by(f64::total_cmp);
        let mut ends = results
            .iter()
            .map(|(_, end)| end.duration_since(start).as_micros() as f64 / 1000.0)
            .collect::<Vec<_>>();
        ends.sort_by(f64::total_cmp);
        let spread = ends[BURST - 1] - ends[0];
        spreads.push(spread);
        all.extend(latencies.iter().copied());
        rounds.push(
            serde_json::json!({"latency_ms": latencies, "answer_at_ms": ends, "spread_ms": spread}),
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let pct = |values: &mut Vec<f64>, p: f64| {
        values.sort_by(f64::total_cmp);
        values[((values.len() as f64 - 1.0) * p).round() as usize]
    };
    // Round 0 warms the daemon; the summary skips it.
    let mut warm_spreads = spreads[1..].to_vec();
    let mut warm_all = all[BURST..].to_vec();
    std::thread::sleep(Duration::from_millis(500));
    // Per-mark step times (the gap since the previous mark) of every traced
    // create after round 0, as p50/max in ms.
    let spans = fs::read_to_string(&spans_path).unwrap_or_default();
    let mut steps: std::collections::BTreeMap<String, Vec<f64>> = Default::default();
    let mut traced = 0;
    for line in spans.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let label = value["label"].as_str().unwrap_or("");
        if label.ends_with(".follow") || label == "host" {
            continue;
        }
        traced += 1;
        let mut previous = 0.0;
        for mark in value["marks"].as_array().into_iter().flatten() {
            let name = mark[0].as_str().unwrap_or("?").to_string();
            let at = mark[1].as_f64().unwrap_or(0.0) / 1000.0;
            steps.entry(format!("{label}:{name}")).or_default().push(at - previous);
            previous = at;
        }
    }
    let step_summary = steps
        .into_iter()
        .map(|(name, mut gaps)| {
            let n = gaps.len();
            (
                name,
                serde_json::json!({"n": n, "p50": pct(&mut gaps, 0.5), "max": pct(&mut gaps, 1.0)}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    panic!(
        "SPLIT-BURST-RESULT {} STEPS traced={traced} {}",
        serde_json::json!({
            "rounds": rounds,
            "spread_p50_ms": pct(&mut warm_spreads, 0.5),
            "spread_max_ms": pct(&mut warm_spreads, 1.0),
            "latency_p50_ms": pct(&mut warm_all, 0.5),
            "latency_p95_ms": pct(&mut warm_all, 0.95),
            "latency_max_ms": pct(&mut warm_all, 1.0),
        }),
        serde_json::Value::Object(step_summary)
    );
}
