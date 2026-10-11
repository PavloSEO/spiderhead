- Inbox references accept goal ids (`inbox:<uuid>`), so a note can cite the goal it concerns.
- Checklist items that were never attempted and are manual show state `not_run` instead of `review`;
  only an attempt awaiting a decision stays in `review`.
- The Issues read model (`observe` findings evidence) reports per-check counts (`by_check`) and a
  `next_offset` after the first 20 findings; `findings_page` pages through the rest.
