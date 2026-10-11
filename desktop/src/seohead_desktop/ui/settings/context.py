"""What the settings sections may ask the application for (kept narrow and explicit)."""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class SettingsContext:
    app_version: str = "0.1.0"
    core_executable: str | None = None
    project_directory: str | None = None
    data_directory: str | None = None
    log_directory: str | None = None
    # name -> callable(**kwargs); sections call context.request("open_logs") and get None when unavailable.
    actions: dict = field(default_factory=dict)

    def request(self, name, **kwargs):
        action = self.actions.get(name)
        return action(**kwargs) if action else None

    def can(self, name):
        return name in self.actions
