//! Input kinds of the command-input catalogue; labels are fixed text used in the reference.

/// Kind of input a form consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputKind {
    /// Live URL.
    LiveUrl,
    /// URL list.
    UrlList,
    /// Domain.
    Domain,
    /// Scan artifact.
    ScanArtifact,
    /// Audit document.
    AuditDocument,
    /// Local directory.
    LegacyDirectory,
    /// Local file.
    LocalFile,
    /// Local log.
    LocalLog,
    /// Inline corpus.
    InlineCorpus,
    /// Inline JSON.
    InlineJson,
    /// Inline HTML.
    InlineHtml,
    /// Inline text.
    InlineText,
    /// Selector.
    Selector,
    /// Provider query.
    ProviderQuery,
    /// Secret from stdin.
    StdinSecret,
    /// Provider identifier.
    ProviderIdentifier,
    /// Local configuration.
    LocalConfig,
    /// Saved crawl profile.
    LocalProfile,
    /// Local directory.
    LocalDirectory,
    /// Project directory.
    ProjectDirectory,
    /// No direct input.
    NoInput,
}

impl InputKind {
    /// Snake-case identifier used in the catalogue data.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LiveUrl => "live_url",
            Self::UrlList => "url_list",
            Self::Domain => "domain",
            Self::ScanArtifact => "scan_artifact",
            Self::AuditDocument => "audit_document",
            Self::LegacyDirectory => "legacy_directory",
            Self::LocalFile => "local_file",
            Self::LocalLog => "local_log",
            Self::InlineCorpus => "inline_corpus",
            Self::InlineJson => "inline_json",
            Self::InlineHtml => "inline_html",
            Self::InlineText => "inline_text",
            Self::Selector => "selector",
            Self::ProviderQuery => "provider_query",
            Self::StdinSecret => "stdin_secret",
            Self::ProviderIdentifier => "provider_identifier",
            Self::LocalConfig => "local_config",
            Self::LocalProfile => "local_profile",
            Self::LocalDirectory => "local_directory",
            Self::ProjectDirectory => "project_directory",
            Self::NoInput => "no_input",
        }
    }

    /// Human label used in the Markdown reference.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LiveUrl => "Live URL",
            Self::UrlList => "URL list",
            Self::Domain => "Domain",
            Self::ScanArtifact => "Scan artifact",
            Self::AuditDocument => "Audit document",
            Self::LegacyDirectory => "Local directory",
            Self::LocalFile => "Local file",
            Self::LocalLog => "Local log",
            Self::InlineCorpus => "Inline corpus",
            Self::InlineJson => "Inline JSON",
            Self::InlineHtml => "Inline HTML",
            Self::InlineText => "Inline text",
            Self::Selector => "Selector",
            Self::ProviderQuery => "Provider query",
            Self::StdinSecret => "Secret from stdin",
            Self::ProviderIdentifier => "Provider identifier",
            Self::LocalConfig => "Local configuration",
            Self::LocalProfile => "Saved crawl profile",
            Self::LocalDirectory => "Local directory",
            Self::ProjectDirectory => "Project directory",
            Self::NoInput => "No direct input",
        }
    }
}
