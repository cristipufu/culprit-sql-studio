//! Reading XML: showplans, deadlock graphs, an IDE's data sources. roxmltree reads nested elements by recursion, and
//! so do the walks over what it returns, so a document nested deep enough exhausts the stack of whichever thread reads
//! it (an unoptimized build overflows 2 MiB at under 200 levels; a pasted file can nest thousands). Every reader goes
//! through [`read`], which refuses a document nested deeper than [`MAX_DEPTH`] before parsing it, and parses and walks
//! the rest on a thread whose stack fits that depth, whatever thread asked.

/// The deepest element nesting read. A showplan's long predicates nest a few hundred levels at most.
pub const MAX_DEPTH: usize = 1024;

/// A stack that fits [`MAX_DEPTH`] levels of parsing and walking in an unoptimized build, with room to spare.
const STACK: usize = 32 * 1024 * 1024;

/// Parse `text` (no DTD) and hand the outcome to `walk`, on a thread sized for it. A document nested deeper than
/// [`MAX_DEPTH`] reaches `walk` as an error, found by a scan that doesn't recurse.
pub fn read<T: Send>(text: &str, walk: impl FnOnce(Result<roxmltree::Document<'_>, String>) -> T + Send) -> T {
    let parse_and_walk = || {
        let doc = if nests_deeper_than(text, MAX_DEPTH) {
            Err(format!("the XML nests elements more than {MAX_DEPTH} deep"))
        } else {
            let options = roxmltree::ParsingOptions { allow_dtd: false, ..roxmltree::ParsingOptions::default() };
            roxmltree::Document::parse_with_options(text, options).map_err(|e| e.to_string())
        };
        walk(doc)
    };
    std::thread::scope(|scope| {
        // Starting a thread fails only when the system is out of threads or memory.
        let worker = std::thread::Builder::new()
            .name("culprit-xml".into())
            .stack_size(STACK)
            .spawn_scoped(scope, parse_and_walk)
            .unwrap_or_else(|e| panic!("could not start a thread to read XML: {e}"));
        worker.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Whether some element in `text` is nested more than `limit` deep: start tags open a level, end tags close one,
/// self-closing tags, comments, CDATA, processing instructions and declarations don't count.
pub fn nests_deeper_than(text: &str, limit: usize) -> bool {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    let skip_to = |from: usize, end: &[u8]| -> usize {
        b[from..].windows(end.len()).position(|w| w == end).map_or(b.len(), |p| from + p + end.len())
    };
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            i = skip_to(i + 4, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i = skip_to(i + 9, b"]]>");
        } else if rest.starts_with(b"<?") {
            i = skip_to(i + 2, b"?>");
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i = skip_to(i + 2, b">");
        } else if rest.starts_with(b"<!") {
            i = skip_to(i + 2, b">");
        } else {
            // A start tag: to its `>`, past quoted attribute values, which may hold `>` and `/`.
            let mut j = i + 1;
            let mut quote = None;
            while j < b.len() {
                match (quote, b[j]) {
                    (Some(q), c) if c == q => quote = None,
                    (None, b'"' | b'\'') => quote = Some(b[j]),
                    (None, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            if b.get(j.wrapping_sub(1)) != Some(&b'/') {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            i = j + 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_counts_elements_not_comments_cdata_or_self_closing_tags() {
        assert!(!nests_deeper_than("<a><b/><c x=\"1 > 0 / 2\"><d></d></c></a>", 3));
        assert!(nests_deeper_than("<a><b><c><d/></c></b></a>", 2));
        assert!(!nests_deeper_than("<a><!-- <b><c><d> --><![CDATA[<e><f>]]><?pi <g>?></a>", 1));
        assert!(!nests_deeper_than("<a></a><a></a><a></a>", 1), "siblings are not nesting");
    }

    #[test]
    fn a_document_nested_deeper_than_the_limit_is_an_error_not_a_stack_overflow() {
        let deep = format!("{}{}", "<a>".repeat(100_000), "</a>".repeat(100_000));
        // On a small stack, as a worker thread might have: the read runs on its own.
        let outcome = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || read(&deep, |doc| doc.map(|d| d.descendants().count())))
            .unwrap()
            .join()
            .unwrap();
        assert!(outcome.unwrap_err().contains("nests elements more than"));

        let fits = format!("{}{}", "<a>".repeat(MAX_DEPTH), "</a>".repeat(MAX_DEPTH));
        let read_back = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || read(&fits, |doc| doc.map(|d| d.descendants().count())))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(read_back, Ok(MAX_DEPTH + 1), "the deepest document read parses on any caller's stack");
    }
}
