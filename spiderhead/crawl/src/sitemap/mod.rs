//! Bounded sitemap parsing of one document (index, urlset or text), plus the shared URL helpers.
//!
//! Stray markup and truncated tags are tolerated, as in a recovering XML parser.

mod crawl;
mod parse;
mod urls;
mod xml;

pub use crawl::*;
pub use parse::*;
pub use urls::*;
pub use xml::Element;
