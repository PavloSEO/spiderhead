# Link qualification and disclosure evidence

Native scans retain the full `rel` token list when `link_attributes.capture=true`.
`scan.v1` stores it in `links.rel_json`; URL link cards already expose the full list
and separate `nofollow`, `sponsored` and `ugc` flags. Capture disabled means these
classes are unmeasured, rather than absent. Link-level qualification and page-level
robots directives remain separate.

`summary.link_rel` counts each qualifying token independently. A mixed edge counts
in every applicable class; `counts_overlap=true` prevents adding these categories
as though they were disjoint. `unqualified` means none of these three tokens was
observed, and does not guarantee indexing or ranking signals.

`MIXED_LINK_REL` is a notice for two or more of `nofollow`, `sponsored` and `ugc` on
one edge. These combinations are valid. The notice asks whether each token is
intentional and preserves unrelated tokens such as `noopener` in the evidence.

`OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE` is a notice from complete retained HTML or
DOM, replayed against the stored link occurrences. It looks for English/Russian
text markers in the nearest block and its preceding sibling label, up to 500 text
characters. Script, template and explicitly hidden text are excluded. An unrelated
footer marker does not disclose a link in another block. Other languages, CSS
visibility, ambiguous text and remote disclosures require manual review: this is
a text-marker heuristic, not a legal-compliance judgement.

Missing, mismatched or oversized retained bodies and truncated local text cannot
prove that disclosure is absent. Coverage reports them as unavailable, and a run
with any unavailable candidates is partial. Successful findings still retain
their source document and link occurrence IDs. Rel capture must be enabled before
the audit evaluates these notices. A legacy in-memory crawl with no retained
document can classify rel tokens but skips the disclosure check by name.

Donor checks expose the same independent `nofollow`, `sponsored`, `ugc` and
`mixed_rel` flags alongside the existing full token list, `follow` and `blocked_by`
fields. Sponsored donor links include the local disclosure heuristic; disclosure
does not change their qualification or page-indexability verdict.
