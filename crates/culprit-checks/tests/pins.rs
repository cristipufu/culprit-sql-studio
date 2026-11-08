//! What a check's findings say, word for word: every part a reader sees, one per line, compared with the English the
//! check said before its words moved into `locales/en.json`. A pin fails on the first line that differs.
//!
//! To fill in a new pin from the code as it is, run its test with `CULPRIT_PIN_RECORD=<dir>`: each pin writes what the
//! findings say to `<dir>/<name>.txt` instead of comparing.

use culprit_core::{Finding, Snapshot};

/// A demo snapshot from `fixtures/`.
pub fn fixture(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

/// Set a setting the way a scan records it, in place of any of that name.
pub fn set(s: &mut Snapshot, name: &str, value: &str) {
    s.settings.retain(|x| !x.name.eq_ignore_ascii_case(name));
    s.settings.push(culprit_core::Setting {
        name: name.into(),
        value: value.into(),
        default_value: None,
        scope: "server".into(),
    });
}

/// The findings one built-in check makes of `s`, as it makes them (before the pipeline refines them).
pub fn run(check: &str, s: &Snapshot) -> Vec<Finding> {
    culprit_checks::all().into_iter().find(|c| c.info().id == check).expect("a built-in check").run(s)
}

/// A built-in check's title and description.
pub fn info(check: &str) -> String {
    let info = culprit_checks::infos().into_iter().find(|c| c.id == check).expect("a built-in check");
    format!("title: {}\ndescription: {}", info.title, info.description)
}

/// Every part of a finding a reader sees, one a line.
pub fn words(f: &Finding) -> String {
    let mut out = vec![
        format!(
            "id: {} · {:?} · {:?} · score {} · share {:?}{}",
            f.id,
            f.severity,
            f.confidence,
            f.impact.score,
            f.impact.db_time_share,
            if f.inactive { " · inactive" } else { "" }
        ),
        format!("title: {}", f.title),
        format!("summary: {}", f.summary),
        format!("impact: {}", f.impact.label),
    ];
    for m in &f.metrics {
        match &m.text {
            Some(text) => out.push(format!("metric: {}: {text}", m.label)),
            None => out.push(format!("metric: {}: {} {:?}", m.label, m.value, m.unit)),
        }
    }
    for q in &f.queries {
        out.push(format!("query: {} {:?} {}", q.id, q.plan_node, q.note.as_deref().unwrap_or("-")));
    }
    if let Some(fix) = &f.fix {
        out.push(format!("fix: {}", fix.summary));
        if let Some(sql) = &fix.sql {
            out.push(format!("sql: {sql}"));
        }
        if let Some(cost) = &fix.cost {
            out.push(format!("cost: {cost}"));
        }
    }
    out.extend(f.verify.iter().map(|v| format!("verify: {v}")));
    out.extend(f.caveats.iter().map(|c| format!("caveat: {c}")));
    out.join("\n")
}

/// `text` is `expected`, word for word; with `CULPRIT_PIN_RECORD` set, it is written there as `<name>.txt` instead.
pub fn pin_text(name: &str, text: &str, expected: &str) {
    if let Some(dir) = std::env::var_os("CULPRIT_PIN_RECORD") {
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.txt")), text).expect("record a pin");
        return;
    }
    if text == expected {
        return;
    }
    let (said, pinned): (Vec<&str>, Vec<&str>) = (text.lines().collect(), expected.lines().collect());
    let at = said.iter().zip(&pinned).position(|(a, b)| a != b).unwrap_or(said.len().min(pinned.len()));
    panic!(
        "{name}: line {} differs\n  said:   {}\n  pinned: {}",
        at + 1,
        said.get(at).unwrap_or(&"<nothing>"),
        pinned.get(at).unwrap_or(&"<nothing>")
    );
}

/// The findings say `expected`, word for word ([`words`], a blank line between findings).
pub fn pin(name: &str, findings: &[Finding], expected: &str) {
    let text = findings.iter().map(words).collect::<Vec<_>>().join("\n\n");
    pin_text(name, &text, expected);
}
