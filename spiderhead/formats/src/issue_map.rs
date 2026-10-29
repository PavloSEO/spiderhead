//! Where the toolkit stands against the field's published crawler catalogue.
//!
//! The catalogue lists the issues a crawler reports: 320 across 24 categories. It is the
//! closest thing technical SEO has to a shared checklist, so it is the honest yardstick for
//! "what does this find". Without it, a count of checks is a number with nothing to compare to.
//!
//! **Only the issue names are used here.** They are the field's shared vocabulary for these
//! problems. Every explanation in this module is written about *our own* behaviour: what we
//! find, what we find only partly, what we do not find, and what we have decided not to find.
//!
//! Each entry carries exactly one status:
//!
//! - `Check`: a registry check finds it; `refs` names the check ids.
//! - `Tool`: a command outside the crawl registry finds it; `refs` names the commands.
//! - `Partial`: we find part of it. `note` says which part we miss.
//! - `Gap`: we should find it and do not. `note` says what is missing.
//! - `OutOfScope`: we will not, and `note` says why it is a decision rather than an oversight.
//!
//! Category rows and the declined-issue reasons live in the `rows` and `declined` submodules as data.

mod declined;
mod rows;

#[cfg(test)]
mod tests;

/// The five statuses an entry can carry, in a fixed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// A registry check finds it.
    Check,
    /// A command outside the crawl registry finds it.
    Tool,
    /// We find part of it.
    Partial,
    /// We should find it and do not.
    Gap,
    /// We will not, and the note says why.
    OutOfScope,
}

impl Status {
    /// Every status, in a fixed order.
    pub const ALL: [Status; 5] = [
        Status::Check,
        Status::Tool,
        Status::Partial,
        Status::Gap,
        Status::OutOfScope,
    ];

    /// The wire name of the status, as written to reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Check => "check",
            Status::Tool => "tool",
            Status::Partial => "partial",
            Status::Gap => "gap",
            Status::OutOfScope => "out_of_scope",
        }
    }
}

/// One catalogue issue with our status for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The issue name, as the catalogue spells it.
    pub name: &'static str,
    /// Our status for the issue.
    pub status: Status,
    /// Check ids (`check`, `partial`), commands (`tool`), or nothing.
    pub refs: &'static [&'static str],
    /// What is found, missed or declined. Empty for a plain check.
    pub note: &'static str,
}

impl Entry {
    /// A registry check finds the issue.
    pub const fn check(name: &'static str, refs: &'static [&'static str]) -> Self {
        Self {
            name,
            status: Status::Check,
            refs,
            note: "",
        }
    }

    /// A command outside the registry finds the issue.
    pub const fn tool(name: &'static str, refs: &'static [&'static str]) -> Self {
        Self {
            name,
            status: Status::Tool,
            refs,
            note: "",
        }
    }

    /// We find part of the issue.
    pub const fn partial(name: &'static str, refs: &'static [&'static str]) -> Self {
        Self {
            name,
            status: Status::Partial,
            refs,
            note: "",
        }
    }

    /// We should find the issue and do not.
    pub const fn gap(name: &'static str) -> Self {
        Self {
            name,
            status: Status::Gap,
            refs: &[],
            note: "",
        }
    }

    /// We will not find the issue; the note gives the reason.
    pub const fn out_of_scope(name: &'static str) -> Self {
        Self {
            name,
            status: Status::OutOfScope,
            refs: &[],
            note: "",
        }
    }

    /// The same entry with its explanatory note set.
    pub const fn note(self, note: &'static str) -> Self {
        Self { note, ..self }
    }
}

/// One catalogue category and its entries, in catalogue order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// The category name, as the catalogue spells it.
    pub name: &'static str,
    /// The entries of the category.
    pub entries: Vec<Entry>,
}

/// Every category, in catalogue order.
pub fn categories() -> Vec<Category> {
    let accessibility = declined::accessibility();
    let amp = declined::amp();
    let pairs: [(&'static str, Vec<Entry>); 24] = [
        ("Response Codes", rows::response_codes()),
        ("Security", rows::security()),
        ("URL", rows::url()),
        ("Page Titles", rows::page_titles()),
        ("Meta Description", rows::meta_description()),
        ("H1", rows::h1()),
        ("H2", rows::h2()),
        ("Content", rows::content()),
        ("Images", rows::images()),
        ("Canonicals", rows::canonicals()),
        ("Pagination", rows::pagination()),
        ("Directives", rows::directives()),
        ("Hreflang", rows::hreflang()),
        ("JavaScript", rows::javascript()),
        ("Links", rows::links()),
        ("Structured Data", rows::structured_data()),
        ("Sitemaps", rows::sitemaps()),
        ("PageSpeed", rows::pagespeed()),
        ("Mobile", rows::mobile()),
        ("Accessibility", accessibility),
        ("Analytics", rows::analytics()),
        ("Search Console", rows::search_console()),
        ("Validation", rows::validation()),
        ("AMP", amp),
    ];
    pairs
        .into_iter()
        .map(|(name, entries)| Category { name, entries })
        .collect()
}

/// Every catalogued issue, as `(category, entry)` pairs in catalogue order.
pub fn entries() -> Vec<(&'static str, Entry)> {
    categories()
        .into_iter()
        .flat_map(|category| {
            let name = category.name;
            category.entries.into_iter().map(move |entry| (name, entry))
        })
        .collect()
}

/// How many catalogue issues carry each status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoverageCounts {
    /// Issues a registry check finds.
    pub check: usize,
    /// Issues a command outside the registry finds.
    pub tool: usize,
    /// Issues we find in part.
    pub partial: usize,
    /// Issues we should find and do not.
    pub gap: usize,
    /// Issues we decline, with a reason.
    pub out_of_scope: usize,
}

impl CoverageCounts {
    /// The count for one status.
    pub const fn get(&self, status: Status) -> usize {
        match status {
            Status::Check => self.check,
            Status::Tool => self.tool,
            Status::Partial => self.partial,
            Status::Gap => self.gap,
            Status::OutOfScope => self.out_of_scope,
        }
    }

    /// The number of catalogued issues across all statuses.
    pub const fn total(&self) -> usize {
        self.check + self.tool + self.partial + self.gap + self.out_of_scope
    }
}

/// Count the catalogue entries by status.
pub fn coverage_counts() -> CoverageCounts {
    let mut counts = CoverageCounts::default();
    for (_, entry) in entries() {
        match entry.status {
            Status::Check => counts.check += 1,
            Status::Tool => counts.tool += 1,
            Status::Partial => counts.partial += 1,
            Status::Gap => counts.gap += 1,
            Status::OutOfScope => counts.out_of_scope += 1,
        }
    }
    counts
}
