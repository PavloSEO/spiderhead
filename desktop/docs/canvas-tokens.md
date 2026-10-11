# Shared desktop canvas tokens

The application theme is the canonical token owner. QSS and native painting read the same values.

| Component | Values |
|---|---|
| Shell | topbar: 52, tabstrip: 38, nav: 200, rail: 64, page header: 64 + 1 border (60 + 1 compact), subtabs: 44 + 1 border |
| Panel | panel_padding_x: 20, panel_padding_y: 18, panel_gap: 14, panel_radius: 14 |
| Calendar | day_size: 40, day_font: 13 |
| Switch | switch_width: 44, switch_height: 26, switch_thumb: 20, switch_thumb_off: 14 |
| Field | field_height: 44, field_radius: 10 |
| Code card | code_radius: 12, code_padding_x: 16, code_padding_y: 14 |
| Mode card | mode_padding: 16, mode_radius: 14 |

| Color role | Value |
|---|---|
| surface | #FDFCFF |
| base | #FFFFFF |
| primary | #1565C0 |
| text | #1C1B1F |
| text_2 | #49454F |
| divider | #E7E4EE |
| outline | #CAC4D0 |
| outline_hover | #79747E |
| canvas_segment_bg | #D9E3F9 |
| canvas_code_bg | #0E1116 |
| canvas_code_fg | #E6EDF7 |
| canvas_icon_primary | #D3E4FF |
| canvas_icon_amber | #FFDCBE |
| canvas_icon_green | #C4ECBB |

The panels have no shadow. Roboto provides regular, medium and bold faces; Roboto Mono serves code. Material Symbols Outlined and Filled are bundled and available offline.

Schedule columns are 1.55:1; Crawl columns are 1.4:1. Compact screens use a single column.
