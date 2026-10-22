"""Export Analyzer — machine-readable SEO audit from crawler exports.

Public surface: :func:`seohead.sf.core.audit.run_audit`, which both the CLI and
the MCP server build on, and the report writers in :mod:`seohead.sf.reporters`.
"""

from seohead import __version__  # One version is shared across the entire toolkit.

__all__ = ["__version__"]
