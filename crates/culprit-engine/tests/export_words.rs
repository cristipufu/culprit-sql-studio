//! What the exports say, word for word: the Markdown and HTML a person saves or shares (a finding, a report, a plan, a
//! comparison, a verification). Their words come from the language catalog (locales/en.json); these tests hold the
//! English to what the exports said when their words were written in code.
//!
//! The browser demo's `fixtures/share.sample.json` already holds a comparison's exports and the first findings' HTML as
//! the code makes them: those are compared with it. The rest are pinned in `tests/words/<name>.txt`; to write a pin
//! from the code as it is, run its test with `CULPRIT_PIN_RECORD=1`.

use std::path::{Path, PathBuf};

use culprit_core::{Report, VerifyStatus, compare_reports, plan_diff_named};
use culprit_engine::export::{self, ExportNote, ExportOptions};

use crate::plan_samples::captured;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn report(name: &str) -> Report {
    serde_json::from_str(&std::fs::read_to_string(root().join("fixtures").join(name)).unwrap()).unwrap()
}

fn share_sample() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("fixtures/share.sample.json")).unwrap()).unwrap()
}

/// An export is stamped with the minute it was made: the rest of it is what has to match.
fn unstamped(text: &str) -> String {
    let mut out = text.to_string();
    let mut from = 0;
    while let Some(at) = out[from..].find("Exported from Culprit on ").map(|at| at + from) {
        let start = at + "Exported from Culprit on ".len();
        let Some(end) = out[start..].find(" UTC").map(|end| end + start) else { break };
        out.replace_range(start..end, "<when>");
        from = start;
    }
    out
}

/// `text` is what `tests/words/<name>.txt` holds, word for word (a page's style sheet left out: it has no words); with
/// `CULPRIT_PIN_RECORD` set, it is written there.
#[track_caller]
pub(crate) fn pin(name: &str, text: &str) {
    let mut text = text.to_string();
    let mut from = 0;
    while let Some(at) = text[from..].find("<style>").map(|at| at + from + "<style>".len()) {
        let Some(end) = text[at..].find("</style>").map(|end| end + at) else { break };
        text.replace_range(at..end, "…");
        from = at;
    }
    let text = text.as_str();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/words").join(format!("{name}.txt"));
    if std::env::var_os("CULPRIT_PIN_RECORD").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let pinned = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if text == pinned {
        return;
    }
    let (said, was): (Vec<&str>, Vec<&str>) = (text.lines().collect(), pinned.lines().collect());
    let at = said.iter().zip(&was).position(|(a, b)| a != b).unwrap_or(said.len().min(was.len()));
    panic!(
        "{name}: line {} differs\n  said:   {}\n  pinned: {}",
        at + 1,
        said.get(at).unwrap_or(&"<nothing>"),
        was.get(at).unwrap_or(&"<nothing>")
    );
}

#[test]
fn the_browser_demos_plan_exports_are_the_exports_of_the_samples() {
    let sample = share_sample();
    let saved = sample["compares"].as_array().expect("compares");
    let options = ExportOptions::default();
    let samples = captured::samples();
    assert_eq!(saved.len(), samples.len());
    for (sample, saved) in samples.into_iter().zip(saved) {
        let [a, b] = sample.plans;
        let [name_a, name_b] = sample.names;
        let diff = plan_diff_named(&a, &name_a, &b, &name_b);
        let said = [
            ("markdown", export::diff_markdown(&a, &b, &diff, &name_a, &name_b, &options)),
            ("html", export::diff_html(&a, &b, &diff, &name_a, &name_b, &options)),
        ];
        for (key, text) in said {
            assert_eq!(unstamped(&text), unstamped(saved[key].as_str().unwrap()), "{name_a} → {name_b}: {key}");
        }
        let plans = [export::plan_markdown(&a, &name_a, &options), export::plan_markdown(&b, &name_b, &options)];
        for (i, text) in plans.iter().enumerate() {
            assert_eq!(unstamped(text), unstamped(saved["plan_markdown"][i].as_str().unwrap()), "{name_a}: plan {i}");
        }
    }
}

#[test]
fn the_browser_demos_finding_pages_are_the_pages_of_the_demo_reports() {
    let sample = share_sample();
    let saved = sample["findings"].as_object().expect("findings");
    let options = ExportOptions::default();
    for file in ["mssql-demo.report.json", "pg-demo.report.json", "mysql-demo.report.json", "mariadb-demo.report.json"]
    {
        let r = report(file);
        for f in r.findings.iter().take(3) {
            let key = format!("{}|{}", r.snapshot.connection, f.id);
            let page = saved[&key]["html"].as_str().unwrap_or_else(|| panic!("{key}: no page"));
            assert_eq!(unstamped(&export::finding_html(&r, f, &options)), unstamped(page), "{key}");
        }
    }
}

/// Every plan page of the captured samples, and the first sample as Markdown and HTML with its values and notes.
#[test]
fn plan_pages_say_what_they_said() {
    let redacted = ExportOptions::default();
    let mut pages = Vec::new();
    for sample in captured::samples() {
        for (plan, name) in sample.plans.iter().zip(&sample.names) {
            pages.push(format!("=== {name}\n{}", unstamped(&export::plan_html(plan, name, &redacted))));
        }
    }
    pin("plan-pages", &pages.join("\n"));
    let sample = captured::samples().into_iter().next().unwrap();
    let plan = &sample.plans[1];
    let node = plan.hottest().map(|n| n.id);
    let raw = ExportOptions {
        redact: false,
        notes: vec![
            ExportNote {
                title: "A note on a step".into(),
                detail: Some("Its detail.".into()),
                fix: Some("Its fix.".into()),
                node,
            },
            ExportNote { title: "A note on the plan".into(), detail: None, fix: None, node: None },
        ],
    };
    pin(
        "plan-raw",
        &format!(
            "{}\n=== html\n{}",
            unstamped(&export::plan_markdown(plan, "A plan", &raw)),
            unstamped(&export::plan_html(plan, "A plan", &raw))
        ),
    );
}

/// The first findings of a report, so the summary table, the findings in full and "the other N findings" all show.
fn shortened(name: &str, keep: usize) -> Report {
    let mut r = report(name);
    r.findings.truncate(keep);
    r
}

/// Formatter coverage must survive ranking changes. Missing cases require an explicit fixture review.
fn finding_samples<'a>(report: &'a Report, ids: &[&str]) -> Vec<&'a culprit_core::Finding> {
    ids.iter()
        .map(|id| {
            report
                .findings
                .iter()
                .find(|finding| finding.id == *id)
                .unwrap_or_else(|| panic!("{}: expected export sample {id} is missing", report.snapshot.connection))
        })
        .collect()
}

#[test]
fn report_exports_say_what_they_said() {
    for (file, name, raws) in [("mssql-demo.report.json", "mssql", true), ("pg-demo.report.json", "pg", false)] {
        let r = shortened(file, 7);
        for (redact, how) in [(true, "redacted"), (false, "raw")].into_iter().filter(|(redact, _)| *redact || raws) {
            let options = ExportOptions { redact, ..Default::default() };
            pin(
                &format!("report-{name}-{how}"),
                &format!(
                    "{}\n=== html\n{}",
                    unstamped(&export::report_markdown(&r, &options)),
                    unstamped(&export::report_html(&r, &options))
                ),
            );
        }
    }
    let options = ExportOptions::default();
    let (none, one) = (shortened("mysql-demo.report.json", 0), shortened("mysql-demo.report.json", 1));
    pin(
        "report-mysql-none-and-one",
        &[
            unstamped(&export::report_markdown(&none, &options)),
            unstamped(&export::report_html(&none, &options)),
            unstamped(&export::report_markdown(&one, &options)),
            unstamped(&export::report_html(&one, &options)),
        ]
        .join("\n=== next\n"),
    );
}

#[test]
fn finding_exports_with_their_values_say_what_they_said() {
    let options = ExportOptions { redact: false, ..Default::default() };
    let mut out = Vec::new();
    let samples: &[(&str, &[&str])] = &[
        ("mssql-demo.report.json", &["failing-query:263", "failing-query:246", "heavy-query:138", "lock-time:143"]),
        ("pg-demo.report.json", &["temp-spill:9065391607317686058", "filtered-scan:public.orders(customer_id)"]),
        (
            "mysql-demo.report.json",
            &["failing-query:4515abd524a46f37", "lock-time:4167fe7f2c20deb7", "heavy-query:4167fe7f2c20deb7"],
        ),
        ("sqlite-demo.report.json", &["unindexed-fk:main.orders(customer_id)"]),
    ];
    for &(file, ids) in samples {
        let r = report(file);
        for f in finding_samples(&r, ids) {
            out.push(format!("=== {file} {}\n{}", f.id, unstamped(&export::finding_markdown(&r, f, &options))));
            out.push(unstamped(&export::finding_html(&r, f, &options)));
        }
    }
    pin("findings-raw", &out.join("\n"));
}

#[test]
fn digest_details_say_what_they_said() {
    let mut out = Vec::new();
    let samples: &[(&str, &[&str])] = &[
        (
            "mssql-demo.report.json",
            &[
                "failing-query:263",
                "failing-query:246",
                "hot-rows:shop.orders",
                "plan-insights:nested-loops:query-263",
                "heavy-query:138",
                "lock-time:138",
                "lock-time:143",
            ],
        ),
        (
            "pg-demo.report.json",
            &[
                "temp-spill:9065391607317686058",
                "filtered-scan:public.orders(customer_id)",
                "planning-time:-967240904657798800",
                "replica-lag:analytics_slot",
            ],
        ),
        (
            "mysql-demo.report.json",
            &[
                "failing-query:4515abd524a46f37",
                "lock-time:4167fe7f2c20deb7",
                "chatty-query:9b30e6ad79d940fe",
                "temp-spill:04cd7be9c366fadc",
                "heavy-query:4167fe7f2c20deb7",
            ],
        ),
    ];
    for &(file, ids) in samples {
        let r = report(file);
        for f in finding_samples(&r, ids) {
            let detail = export::finding_detail(&r, f);
            out.push(format!("=== {file} {}", f.id));
            out.push(serde_json::to_string_pretty(&detail).unwrap());
        }
    }
    pin("digest-details", &out.join("\n"));
}

#[test]
fn verification_markdown_says_what_it_said() {
    let mut out = Vec::new();
    for file in ["mssql-demo.report.json", "pg-demo.report.json"] {
        let before = report(file);
        let mut after = before.clone();
        let at = chrono::DateTime::parse_from_rfc3339(&after.snapshot.taken_at).unwrap() + chrono::Duration::hours(1);
        after.snapshot.taken_at = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let gone = after.findings.remove(0);
        if let Some(assessed) = after.assessments.iter_mut().find(|a| a.check == gone.check) {
            assessed.status = culprit_core::CheckStatus::Assessed;
            assessed.reason = None;
        }
        let mut added = after.findings[0].clone();
        added.id = "missing-index:shop.brand_new(col)".into();
        added.title = "A new finding".into();
        after.findings.push(added);
        let mut v = compare_reports(&before, &after, &[]);
        v.findings.push(culprit_core::VerifiedFinding {
            id: "no-such:id".into(),
            title: "Something unknown".into(),
            status: VerifyStatus::Unknown,
            before: Some("12 ms".into()),
            after: None,
            note: Some("A note about it.".into()),
        });
        out.push(format!("=== {file}\n{}", export::verification_markdown(&v)));
    }
    pin("verification", &out.join("\n"));
}
