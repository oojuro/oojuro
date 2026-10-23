//! Markdown comparison of scorecards, grouped by suite / dataset / split.
//! Every group states hardware, run conditions and each arm's durability.
use crate::scorecard::{ArmResult, Scorecard, Stat};
use serde_json::Value;
use std::{collections::BTreeMap, fmt::Write};

fn primary(suite: &str) -> &'static str {
    match suite {
        "memqa" => "turn_ndcg@10",
        _ => "ndcg@10",
    }
}

fn quality_order(suite: &str) -> &'static [&'static str] {
    match suite {
        "beir" => &["ndcg@10", "recall@10", "recall@100", "mrr@10"],
        "turnloop" => &["ndcg@10", "ndcg@20", "recall@20", "hit@20", "mrr@10", "freshness_self_hit@21"],
        "memqa" => &["turn_ndcg@10", "turn_recall@10", "turn_recall@50", "session_recall_all@5", "session_recall_any@5"],
        _ => &[],
    }
}

fn speed_order(suite: &str) -> &'static [&'static str] {
    match suite {
        "beir" => &["build_s", "search_get_p50_us", "search_get_p99_us", "search_p50_us", "search_p99_us"],
        "turnloop" => &["prefill_s", "turn_content_overhead_p50_us", "turn_content_overhead_p99_us", "search_get_p50_us", "search_get_p99_us", "append_p50_us", "append_p99_us"],
        "memqa" => &["build_ms_p50", "search_p50_us", "search_p99_us"],
        _ => &[],
    }
}

fn fmt(stat: Option<&Stat>, digits: usize) -> String {
    match stat {
        None => "–".into(),
        Some(s) if s.n == 0 => "–".into(),
        Some(s) if (s.max - s.min).abs() <= f64::EPSILON * s.median.abs().max(1.0) => format!("{:.*}", digits, s.median),
        Some(s) => format!("{:.*} ({:.*}–{:.*})", digits, s.median, digits, s.min, digits, s.max),
    }
}

fn digits(name: &str) -> usize {
    if name.ends_with("_us") || name.ends_with("_s") || name.ends_with("_ms_p50") || name.ends_with("_ms") { 1 } else { 3 }
}

fn gates(a: &ArmResult) -> String {
    if let Some(e) = &a.error {
        return format!("ERROR: {}", e.chars().take(80).collect::<String>());
    }
    if a.gates.is_empty() {
        return "–".into();
    }
    let failed: Vec<&str> = a.gates.iter().filter(|(_, g)| !g.pass).map(|(n, _)| n.as_str()).collect();
    if failed.is_empty() { format!("pass ({})", a.gates.len()) } else { format!("FAIL: {}", failed.join(", ")) }
}

fn ratio(map: &BTreeMap<String, Stat>, name: &str) -> String {
    match map.get(name) {
        Some(s) if s.n > 0 => format!("{:.2}×", s.median),
        _ => "–".into(),
    }
}

pub fn render(cards: &[Scorecard], baseline: Option<&str>) -> String {
    let mut groups: Vec<((String, String, String), Vec<&Scorecard>)> = Vec::new();
    for c in cards {
        let key = (c.suite.clone(), c.dataset.clone(), c.split.clone());
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(c),
            None => groups.push((key, vec![c])),
        }
    }
    let mut out = String::from("# Oojuro benchmark report\n\n");
    for ((suite, dataset, split), cards) in groups {
        let arms: Vec<&ArmResult> = cards.iter().flat_map(|c| &c.arms).collect();
        let first = cards[0];
        let _ = writeln!(out, "## {suite} · {dataset} · {split}\n");
        let hw = &first.hardware;
        let _ = writeln!(
            out,
            "- Hardware: {} · {} logical CPUs · {} RAM · {} · rustc {}",
            hw["cpu"].as_str().unwrap_or("?"),
            hw["logical_cpus"],
            hw["mem_total"].as_str().unwrap_or("?"),
            hw["work_filesystem"]["fstype"].as_str().map_or("?".into(), |f| format!("store on {f}")),
            hw["rustc"].as_str().unwrap_or("?"),
        );
        for key in [
            "runs", "cache", "build_time_included", "isolation", "k", "queries", "documents", "trajectories", "steps",
            "labelled_steps", "background_target_records", "thinking", "questions", "returned_content",
        ] {
            if let Some(v) = first.settings.get(key) {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let _ = writeln!(out, "- {key}: {v}");
            }
        }
        let _ = writeln!(out, "- scorecards: {}\n", cards.iter().map(|c| c.created_at.as_str()).collect::<Vec<_>>().join(", "));
        let mut quality: Vec<String> = quality_order(&suite).iter().map(|s| s.to_string()).collect();
        for a in &arms {
            for k in a.quality.keys() {
                if !quality.contains(k) {
                    quality.push(k.clone());
                }
            }
        }
        quality.retain(|q| arms.iter().any(|a| a.quality.contains_key(q)));
        let speed: Vec<&str> = speed_order(&suite).iter().copied().filter(|s| arms.iter().any(|a| a.speed.contains_key(*s))).collect();
        let base = baseline
            .and_then(|b| arms.iter().find(|a| a.label == b))
            .and_then(|a| a.quality.get(primary(&suite)))
            .map(|s| s.median);
        let mut header = vec!["arm".to_string(), "gates".into()];
        header.extend(quality.iter().cloned());
        if base.is_some() {
            header.push(format!("Δ{} vs {}", primary(&suite), baseline.unwrap_or_default()));
        }
        header.extend(["disk/raw".into(), "peak mem/raw".into(), "steady RSS/raw".into()]);
        header.extend(speed.iter().map(|s| s.to_string()));
        let _ = writeln!(out, "| {} |", header.join(" | "));
        let _ = writeln!(out, "|{}|", vec!["---"; header.len()].join("|"));
        for a in &arms {
            let mut row = vec![format!("{} `{}`", a.label, a.info.name), gates(a)];
            row.extend(quality.iter().map(|q| fmt(a.quality.get(q), 3)));
            if let Some(b) = base {
                row.push(match a.quality.get(primary(&suite)) {
                    Some(s) if s.n > 0 && b != 0.0 => format!("{:+.1}%", (s.median / b - 1.0) * 100.0),
                    _ => "–".into(),
                });
            }
            row.push(ratio(&a.storage, "disk_ratio"));
            row.push(ratio(&a.memory, "peak_cgroup_ratio"));
            row.push(ratio(&a.memory, "steady_rss_ratio"));
            row.extend(speed.iter().map(|s| fmt(a.speed.get(*s), digits(s))));
            let _ = writeln!(out, "| {} |", row.join(" | "));
        }
        let mut slices: Vec<&String> = arms.iter().flat_map(|a| a.breakdown.keys()).collect();
        slices.sort();
        slices.dedup();
        if !slices.is_empty() {
            let metric = primary(&suite);
            let _ = writeln!(out, "\n{metric} by slice:\n");
            let _ = writeln!(out, "| slice | {} |", arms.iter().map(|a| a.label.as_str()).collect::<Vec<_>>().join(" | "));
            let _ = writeln!(out, "|---|{}|", vec!["---"; arms.len()].join("|"));
            for g in slices {
                let cells: Vec<String> = arms.iter().map(|a| fmt(a.breakdown.get(g).and_then(|m| m.get(metric)), 3)).collect();
                let _ = writeln!(out, "| {g} | {} |", cells.join(" | "));
            }
        }
        let _ = writeln!(out, "\nDurability per arm:\n");
        for a in &arms {
            let _ = writeln!(out, "- {}: {} ({}; commit {})", a.label, a.info.durability, a.info.version, a.info.commit);
            for note in &a.info.notes {
                let _ = writeln!(out, "  - {note}");
            }
        }
        let refs: Vec<&Value> = cards.iter().flat_map(|c| &c.references).collect();
        if !refs.is_empty() {
            let _ = writeln!(out, "\nPublished references (not run here; full test split):\n");
            let _ = writeln!(out, "| system | metric | value | uses LLM | source |");
            let _ = writeln!(out, "|---|---|---|---|---|");
            for r in refs {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} |",
                    r["system"].as_str().unwrap_or("?"),
                    r["metric"].as_str().unwrap_or("?"),
                    r["value"],
                    r["uses_llm"].as_bool().map_or("?", |b| if b { "yes" } else { "no" }),
                    r["source"].as_str().unwrap_or("?"),
                );
            }
        }
        for n in cards.iter().flat_map(|c| &c.notes) {
            let _ = writeln!(out, "\nNote: {n}");
        }
        out.push('\n');
    }
    out
}
