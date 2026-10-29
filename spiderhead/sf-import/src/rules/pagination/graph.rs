//! The rel="next" graph helpers: pure walks (`series_from`,
//! `pagination_loops`), page-number extraction and the judged/unjudged
//! reasoning shared by the series and sequence checks.

use std::collections::HashSet;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use spiderhead_parse::py_str::py_strip;
use spiderhead_parse::py_url::urlsplit;

use crate::context::{AuditContext, ContextError};
use crate::normalize::norm_url;
use crate::rules::shared::parse_qsl_keep_blanks;

/// Value of one Unicode decimal digit (category Nd). Nd characters come in
/// runs of ten starting at a zero, so the value is the offset into the run.
fn decimal_digit(c: char) -> Option<i128> {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    if get_general_category(c) != G::DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while start > 0 {
        match char::from_u32(start - 1) {
            Some(prev) if get_general_category(prev) == G::DecimalNumber => start -= 1,
            _ => break,
        }
    }
    Some(i128::from(c as u32 - start) % 10)
}

/// reference `value.isdecimal()` then `int(value)`: every character must be a
/// Unicode decimal digit and the run must be non-empty. Values above `i128`
/// have no Rust representation (reference bigints are unbounded) and read as
/// unnumbered here — a bound no real pagination reaches.
fn decimal_int(text: &str) -> Option<i128> {
    if text.is_empty() {
        return None;
    }
    text.chars().try_fold(0i128, |acc, c| {
        acc.checked_mul(10)?.checked_add(decimal_digit(c)?)
    })
}

/// `series_from`: the forward rel="next" chain from `start`, and the node it
/// loops back to.
///
/// Terminates without a hop cap: the graph is functional — one outgoing edge
/// its own path, and it cannot be longer than the number of distinct nodes.
pub fn series_from(
    next_map: &IndexMap<String, String>,
    start: &str,
) -> (Vec<String>, Option<String>) {
    let mut path = vec![start.to_owned()];
    let mut seen: HashSet<String> = HashSet::from([start.to_owned()]);
    let mut cur = start.to_owned();
    loop {
        let Some(nxt) = next_map.get(&cur) else {
            return (path, None);
        };
        if seen.contains(nxt) {
            return (path, Some(nxt.clone()));
        }
        path.push(nxt.clone());
        seen.insert(nxt.clone());
        cur = nxt.clone();
    }
}

/// One rel="next" cycle: `(start, path, loops_back_to)`.
pub type PaginationLoop = (String, Vec<String>, String);

/// `pagination_loops`: every rel="next" cycle in a functional graph, visiting
/// each node once. Returns `(loops, nodes_in_a_loop)` where each loop is a
/// [`PaginationLoop`].
///
/// Module-level and pure so the property that matters can be tested directly:
/// the total number of nodes appended across all walks equals the number of
/// nodes in the graph. Bounding each walk by the graph's own size instead is
/// correct and quadratic — for one terminating series of n pages, each of the
/// n nodes re-walks the rest of the tail.
pub fn pagination_loops(
    next_map: &IndexMap<String, String>,
) -> (Vec<PaginationLoop>, HashSet<String>) {
    let mut walked: HashSet<String> = HashSet::new();
    let mut in_loop: HashSet<String> = HashSet::new();
    let mut loops = Vec::new();
    for start in next_map.keys() {
        if walked.contains(start) {
            continue;
        }
        let (path, loops_to) = series_from(next_map, start);
        walked.extend(path.iter().cloned());
        if let Some(loops_to) = loops_to {
            in_loop.extend(path.iter().cloned());
            loops.push((start.clone(), path, loops_to));
        }
    }
    (loops, in_loop)
}

// A page number is only read from a token that says it is one. A bare trailing
// number in a path is a year, a product id or a date just as often as a page
// index, and guessing wrong here does not produce a near-miss -- it produces a
// sequence "error" on a series that is perfectly ordered. These three tokens
// are the ones that mean "page N" and nothing else. WordPress's ``?p=`` is
// deliberately absent: it carries a post id, not a page index.
/// `_PAGE_NUMBER_PARAMS`.
const PAGE_NUMBER_PARAMS: [&str; 3] = ["page", "paged", "pg"];

/// `_PAGE_NUMBER_PATH_RE`.
static PAGE_NUMBER_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)/(?:page|paged|pg)/(\d{1,6})(?:/|$)").expect("page-number re")
});

/// `pagination_page_number`: the page number `url` states, or `None` when it
/// does not state one.
///
/// Two *disagreeing* numbers (`/page/2/` that also carries `?page=5`) return
/// `None` for the same reason a missing one does: the URL does not tell us
/// which page it is, and a check that picks one of them is inventing the
/// evidence it then reports on.
pub fn pagination_page_number(url: &str) -> Option<i128> {
    let Ok(parts) = urlsplit(url) else {
        return None;
    };
    let mut seen: HashSet<i128> = HashSet::new();
    for (key, value) in parse_qsl_keep_blanks(&parts.query) {
        if PAGE_NUMBER_PARAMS.contains(&py_strip(&key).to_lowercase().as_str()) {
            let n = decimal_int(&value)?;
            seen.insert(n);
        }
    }
    if let Some(m) = PAGE_NUMBER_PATH_RE.captures(&parts.path) {
        // reference `\d` matches Unicode decimal digits, and `int()` reads them —
        // both handled by `decimal_int`.
        let n = decimal_int(m.get(1)?.as_str())?;
        seen.insert(n);
    }
    if seen.len() == 1 {
        seen.into_iter().next()
    } else {
        None
    }
}

/// `_display_url`: the crawled URL behind a normalized key, or the key itself
/// when uncrawled.
pub(crate) fn display_url(ctx: &mut AuditContext, norm: &str) -> Result<String, ContextError> {
    if let Some(page) = ctx.page_by_url(norm) {
        return Ok(page.url());
    }
    let key = norm_url(Some(norm))?;
    Ok(match ctx.page_by_norm(&key) {
        Some(page) => page.url(),
        None => norm.to_owned(),
    })
}

/// `_expected_step`: the step a series is judged against, or `None` when the
/// steps show none.
///
/// A run that ever steps by one is judged on +1, as it always was. Otherwise
/// the series is a stride, but only when one step is at least two and accounts
/// for a strict majority of the steps: a tie or a lone step is not evidence of
/// an offset scheme, so it is left unjudged rather than given a stride it may
/// not have.
pub(crate) fn expected_step(steps: &[i128]) -> Option<i128> {
    if steps.is_empty() {
        return None;
    }
    if steps.contains(&1) {
        return Some(1);
    }
    let mut counts: IndexMap<i128, usize> = IndexMap::new();
    for step in steps {
        *counts.entry(*step).or_insert(0) += 1;
    }
    // `Counter.most_common(1)` resolves ties by first-seen order.
    let (step, count) = counts.iter().fold(
        (0, 0),
        |best, (s, c)| if *c > best.1 { (*s, *c) } else { best },
    );
    (step >= 2 && 2 * count > steps.len()).then_some(step)
}

/// `_series_unjudged_reason`: why this series cannot be judged for a broken
/// run, or `None` when it can.
///
/// One clause per cause, each a statement about *this* series, because the
/// causes are not interchangeable. The clause is written to follow
/// "N series where ...".
pub(crate) fn series_unjudged_reason(stated: &[Option<i128>], cycles: bool) -> Option<String> {
    if cycles {
        return Some(
            "the series cycles back on itself, which PAGINATION_LOOP reports instead of this check"
                .to_owned(),
        );
    }
    // Three numbered pages is the smallest series with two steps, and two steps
    // is the smallest run in which one of them can be "the odd one".
    if stated.len() < 3 {
        return Some(format!(
            "the series is only {} pages long, and a single step cannot be \
             both the run and the break in it",
            stated.len()
        ));
    }
    let unnumbered = stated.iter().filter(|n| n.is_none()).count();
    if unnumbered > 0 {
        let states = if unnumbered == 1 { "states" } else { "state" };
        return Some(format!(
            "{unnumbered} of its {} URLs {states} no page number, and a \
             missing number is not evidence of a gap",
            stated.len()
        ));
    }
    let numbers: Vec<i128> = stated.iter().flatten().copied().collect();
    let steps: Vec<i128> = numbers.windows(2).map(|w| w[1] - w[0]).collect();
    if expected_step(&steps).is_none() {
        return Some(
            "its page numbers neither step by one nor by one stride that covers most \
             of the series, so there is no run to have been broken (an irregular offset \
             scheme such as 0, 10, 25, 30 looks like this)"
                .to_owned(),
        );
    }
    None
}

/// `_unjudged_series_skip_reason`: one skip reason covering every series that
/// went unjudged, grouped by cause.
///
/// Grouped rather than listed one per line because the causes are bounded and
/// the series are not.
pub(crate) fn unjudged_series_skip_reason(unjudged: &[(String, String)], judged: usize) -> String {
    let mut by_reason: IndexMap<String, Vec<String>> = IndexMap::new();
    for (head, reason) in unjudged {
        by_reason
            .entry(reason.clone())
            .or_default()
            .push(head.clone());
    }
    let parts = by_reason
        .iter()
        .map(|(reason, heads)| format!("{} where {} (e.g. {})", heads.len(), reason, heads[0]))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{} of {} rel=\"next\" series could not be judged: {}",
        unjudged.len(),
        unjudged.len() + judged,
        parts
    )
}
