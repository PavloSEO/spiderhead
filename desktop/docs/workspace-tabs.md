# Workspace tab behavior

The strip keeps up to 12 immutable `WorkspaceContext` descriptors. The host owns
the shared gateway, scan manager and project views. Renaming, pinning and moving
tabs update presentation data only; closing emits a request to the host and does
not stop a scan.

- The plus button follows the visible tab strip. Extra window width stays after
  it; native scroll controls and the all-tabs menu keep overflow reachable.
- A new context opens immediately after the active tab. `add(after_id=...)`
  places a duplicate after its source. An unpinned context opened from a pinned
  tab starts the unpinned group.
- Right-click a tab for Rename, Pin/Unpin, Duplicate and Close. Double-click also
  opens Rename. An empty name restores the automatic project/scan title.
- Pinned tabs stay before unpinned tabs. Native dragging reorders within each
  group; a drag across its boundary stops at the edge. Pinning preserves the
  active context ID.
- Pinned tabs have no active close button, and Cmd/Ctrl W ignores them. Their
  explicit menu action can close them. `remove(id)` remains a host operation,
  including an explicitly requested agent close.
- Cmd/Ctrl T, W and the native next/previous-tab bindings keep their existing
  window-level behavior while a page editor has focus.

`display_alias: str | None` and `pinned: bool` belong to the immutable context,
beside project and scan identity. `set_alias` and `set_pinned` replace that
descriptor. Host `update_context`, `update_state` and automatic `update_title`
calls preserve the alias and pin state; a duplicate should copy the alias and
start unpinned. `contexts()` returns descriptors in visual order. These fields
follow the existing in-memory context lifetime and do not create project files
or a new persistence store.

`tests/test_workspace_tabs.py` covers stable identity, insertion order, immutable
capture, rename cancellation/reset, pin boundaries, close guards, shared-service
isolation, capacity, keyboard actions and plus-button geometry. Offscreen tests
do not replace native platform visual acceptance.
