- Server logs now retain exact disk-backed bot-IP totals instead of a capped sample,
  with bounded per-IP records, visits, last visit and subnet filters. Explicit DNS
  verification checks each selected IP separately, records PTR and forward evidence,
  supports IPv6, and preserves unavailable/deadline outcomes as unchecked. Local
  caches expire after one hour; reads never trigger DNS.
- Added local project log import, paged summary, bot-evidence and native-scan join
  commands through the shared CLI/MCP handlers. Separate log artifacts retain date
  partitions and source coverage/provenance without changing frozen scan schemas.
  Missing or incomplete evidence withholds absence conclusions. Desktop log screens
  remain a separate presentation follow-up.
