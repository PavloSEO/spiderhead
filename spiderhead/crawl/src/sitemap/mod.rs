//! Bounded sitemap parsing of one document (index, urlset or text), plus the shared URL helpers.
//!
//! Stray markup and truncated tags are tolerated, as in a recovering XML parser.
//! The networked walk streams well-formed documents entry by entry (quick-xml),
//! and keeps the tolerant document parse for anything else.

mod crawl;
mod parse;
mod stream;
mod urls;
mod xml;

pub use crawl::*;
pub use parse::*;
pub use urls::*;
pub use xml::Element;
