//! Stable paths in wording assertions. Only known test roots and their path suffixes are normalized;
//! backslashes in SQL, connection strings and other output remain significant.

use std::path::Path;

pub fn normalize(text: &str, roots: &[(&Path, &str)]) -> String {
    let mut replacements = Vec::new();
    for (root, label) in roots {
        replacements.push((root.display().to_string(), *label));
        if let Ok(canonical) = root.canonicalize() {
            replacements.push((canonical.display().to_string(), *label));
        }
    }
    // Canonical roots can contain a prefix (/private on macOS or \\?\ on Windows).
    // Replace them before the shorter spelling can match inside them.
    replacements.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    let mut normalized = text.to_string();
    for (path, label) in replacements {
        normalized = normalized.replace(&path, label);
    }
    normalized
        .split_inclusive(char::is_whitespace)
        .map(|part| {
            if roots.iter().any(|(_, label)| part.contains(label)) { part.replace('\\', "/") } else { part.to_string() }
        })
        .collect()
}

#[test]
fn only_test_paths_lose_platform_specific_separators() {
    let text = r"read C:\work\repo\fixtures\plan.json and SELECT '\\server\share'";
    assert_eq!(
        normalize(text, &[(Path::new(r"C:\work\repo"), "<repo>")]),
        r"read <repo>/fixtures/plan.json and SELECT '\\server\share'"
    );
}
