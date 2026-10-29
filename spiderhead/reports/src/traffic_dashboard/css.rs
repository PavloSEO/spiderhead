//! The inline `<style>` block, parameterised by the brand colours.

use super::{mix, Brand};

/// The whole stylesheet, parameterised by the brand colours.
pub(super) fn css(brand: &Brand) -> String {
    let stripe = mix(&brand.card, &brand.ink, 0.03);
    let border = mix(&brand.table_header, "#FFFFFF", 0.45);
    let font_stack = &brand.font_stack;
    let ink = &brand.ink;
    let accent = &brand.accent;
    let positive = &brand.positive;
    let negative = &brand.negative;
    let table_header = &brand.table_header;
    format!(
        r#"
@page {{ size: 297mm 210mm; margin: 0; }}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; padding: 0; background: #FFFFFF; }}
body {{ font-family: {font_stack}; color: {ink}; font-size: 9pt; line-height: 1.35;
  -webkit-print-color-adjust: exact; print-color-adjust: exact; }}
.page {{ width: 297mm; height: 210mm; padding: 11mm 13mm 8mm; display: flex; flex-direction: column;
  overflow: hidden; position: relative; break-after: page; page-break-after: always;
  background: #FFFFFF; }}
.page:last-child {{ break-after: auto; page-break-after: auto; }}
.page-head {{ display: flex; justify-content: space-between; align-items: flex-start; gap: 8mm;
  margin-bottom: 5mm; flex: none; }}
.eyebrow {{ font-size: 7.5pt; letter-spacing: .09em; text-transform: uppercase; font-weight: 700;
  color: {accent}; margin-bottom: 1.2mm; }}
h2 {{ font-size: 18pt; line-height: 1.15; margin: 0; font-weight: 800; color: {ink};
  max-width: 190mm; }}
.period {{ flex: none; background: #FFFFFF; border-radius: 1.6mm; padding: 2.4mm 4.5mm;
  font-size: 9pt; white-space: nowrap; box-shadow: 0 0.5mm 1.8mm rgba(15,23,42,.18),
  0 0 0 0.2mm rgba(15,23,42,.06); }}
.page-body {{ flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 4.5mm; }}
.page-foot {{ flex: none; display: flex; justify-content: space-between; font-size: 7pt;
  color: #64748B; border-top: 0.25mm solid #E2E8F0; padding-top: 1.8mm; margin-top: 4mm; }}
.card {{ background: #FFFFFF; border-radius: 2.4mm; box-shadow: 0 0.6mm 2.4mm rgba(15,23,42,.10),
  0 0 0 0.2mm rgba(15,23,42,.07); }}
.kpis {{ display: grid; grid-template-columns: repeat(3, 1fr); gap: 3.6mm; flex: none; }}
.kpis.four {{ grid-template-columns: repeat(4, 1fr); }}
.kpi {{ padding: 2.8mm 4.4mm 2.6mm; border-left: 1.1mm solid {accent}; min-width: 0; }}
.kpi.off {{ border-left-color: #CBD5E1; }}
.kpi-label {{ font-size: 8pt; color: #64748B; font-weight: 600; }}
.kpi-value {{ font-size: 18pt; font-weight: 750; line-height: 1.15; margin: 0.6mm 0 0.8mm;
  font-variant-numeric: tabular-nums; }}
.kpi.big {{ padding: 4mm 5mm; }}
.kpi.big .kpi-value {{ font-size: 25pt; }}
.kpi-delta, .kpi-sub {{ font-size: 8pt; display: flex; align-items: baseline; gap: 1.6mm;
  white-space: nowrap; overflow: hidden; }}
.kpi-sub {{ margin-top: 0.5mm; opacity: .85; }}
.kpi.off .kpi-sub {{ white-space: normal; color: #64748B; }}
.vs {{ color: #64748B; font-weight: 400; }}
.delta {{ font-weight: 700; white-space: nowrap; font-variant-numeric: tabular-nums; }}
.delta i {{ font-style: normal; font-size: 6.5pt; margin-right: 0.8mm; position: relative;
  top: -0.2mm; }}
.delta.good {{ color: {positive}; }}
.delta.bad {{ color: {negative}; }}
.delta.none, .delta.flat {{ color: #94A3B8; font-weight: 600; }}
.delta.new {{ color: {accent}; font-weight: 700; }}
.two {{ display: grid; grid-template-columns: 1fr 1fr; gap: 5mm; min-height: 0; }}
.split {{ display: grid; grid-template-columns: 1.25fr 1fr; gap: 5mm; min-height: 0;
  align-items: start; }}
.stack {{ display: flex; flex-direction: column; gap: 4mm; min-width: 0; }}
.table-card {{ overflow: hidden; align-self: start; }}
table.data {{ width: 100%; border-collapse: separate; border-spacing: 0; table-layout: fixed;
  font-size: 8.4pt; font-variant-numeric: tabular-nums; }}
.data col.c-num {{ width: 9mm; }}
.data col.c-val {{ width: 22mm; }}
.data col.c-share {{ width: 19mm; }}
.data col.c-delta {{ width: 22mm; }}
.data thead th {{ background: {table_header}; color: {ink}; font-weight: 700;
  text-align: left; padding: 1.9mm 2.6mm; font-size: 8.6pt; }}
.data th.num, .data td.num {{ text-align: right; }}
.data td {{ padding: 1.35mm 2.6mm; border-bottom: 0.25mm solid {border}; white-space: nowrap;
  overflow: hidden; text-overflow: ellipsis; }}
.data tbody tr:nth-child(even) td {{ background: {stripe}; }}
.data td.idx, .data th.idx {{ color: #64748B; padding-right: 1mm; }}
.data td.muted {{ color: #64748B; }}
.data tfoot td {{ font-weight: 750; padding: 1.9mm 2.6mm; border-top: 0.35mm solid {ink};
  border-bottom: none; background: #FFFFFF; }}
.data.wide {{ font-size: 8.8pt; }}
.data.wide td {{ padding-top: 2.05mm; padding-bottom: 2.05mm; }}
.data.roomy td {{ padding-top: 2.7mm; padding-bottom: 2.7mm; font-size: 9.2pt; }}
.data.gsc td {{ padding-top: 1.25mm; padding-bottom: 1.25mm; }}
.panel {{ padding: 3.2mm 4mm 2.6mm; min-width: 0; }}
.panel.grow {{ flex: 1; min-height: 0; }}
.panel svg {{ display: block; width: 100%; height: auto; }}
.panel-head {{ display: flex; justify-content: space-between; align-items: center;
  margin-bottom: 1mm; }}
.panel-title {{ font-size: 9.5pt; font-weight: 700; margin-bottom: 2mm; }}
.legend {{ display: flex; flex-wrap: wrap; gap: 2mm 5mm; font-size: 8.4pt; }}
.chip {{ display: inline-flex; align-items: center; gap: 1.6mm; white-space: nowrap; }}
.chip i {{ display: inline-block; width: 5.5mm; height: 2.4mm; border-radius: 0.6mm; }}
.donut-panel {{ display: flex; flex-direction: column; }}
.donut-row {{ display: flex; align-items: center; gap: 6mm; }}
.donut {{ flex: none; }}
.donut svg {{ display: block; width: 100%; height: auto; }}
.donut-legend {{ list-style: none; margin: 0; padding: 0; flex: 1; min-width: 0;
  font-size: 8.4pt; }}
.donut-legend li {{ display: flex; align-items: center; gap: 2mm; padding: 0.55mm 0;
  border-bottom: 0.2mm solid #F1F5F9; }}
.donut-legend i {{ flex: none; width: 2.8mm; height: 2.8mm; border-radius: 50%; }}
.lg-name {{ flex: 1; min-width: 0; line-height: 1.2; overflow: hidden; }}
.lg-val {{ flex: none; color: #475569; font-variant-numeric: tabular-nums; }}
.note {{ padding: 4mm 5mm; min-width: 0; }}
.note h3 {{ margin: 0 0 2.4mm; font-size: 10.5pt; color: {ink}; }}
.note ul {{ margin: 0; padding-left: 4.5mm; }}
.note li {{ margin: 0 0 1.8mm; font-size: 9pt; }}
.note li::marker {{ color: {accent}; }}
.note.warn {{ border-left: 1.1mm solid {negative}; }}
.unavailable {{ padding: 6mm; color: #475569; background: #F8FAFC;
  border: 0.3mm dashed #CBD5E1; box-shadow: none; }}
.unavailable.tall {{ flex: 1; display: flex; flex-direction: column; justify-content: center;
  align-items: center; text-align: center; }}
.ua-title {{ font-weight: 700; font-size: 11pt; color: {ink}; margin-bottom: 1.5mm; }}
.ua-reason {{ font-size: 8.5pt; }}
.footnote {{ font-size: 7.8pt; color: #64748B; }}
.footnote.top {{ margin-bottom: -1mm; }}
.cover {{ padding: 0; justify-content: center; }}
.cover-bar {{ position: absolute; left: 0; top: 0; bottom: 0; width: 9mm;
  background: {accent}; }}
.cover-inner {{ padding: 0 30mm 0 34mm; }}
.logo {{ display: inline-block; font-weight: 800; letter-spacing: .06em; font-size: 11pt;
  color: #FFFFFF; background: {ink}; padding: 2mm 4mm; border-radius: 1.2mm;
  margin-bottom: 12mm; }}
.cover h1 {{ font-size: 40pt; line-height: 1.08; margin: 0; font-weight: 800;
  color: {ink}; max-width: 220mm; }}
.cover-site {{ font-size: 28pt; font-weight: 800; color: {accent}; margin-top: 6mm; }}
.cover-meta {{ margin-top: 16mm; font-size: 10pt; color: #475569; display: flex;
  flex-direction: column; gap: 2mm; border-top: 0.3mm solid #E2E8F0; padding-top: 6mm;
  max-width: 200mm; }}
.cover-meta span {{ display: block; font-size: 8pt; text-transform: uppercase;
  letter-spacing: .08em; color: #94A3B8; }}
.cover-meta b {{ font-size: 14pt; color: {ink}; }}
"#
    )
}
