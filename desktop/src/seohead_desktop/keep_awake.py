"""Keep the Mac awake only while a scheduled scan runs: a caffeinate process that exists for as long as it is wanted."""

from __future__ import annotations

import shutil
import subprocess


class KeepAwake:
    def __init__(self, which=shutil.which, popen=subprocess.Popen):
        self._which = which
        self._popen = popen
        self._process = None

    @property
    def active(self):
        return self._process is not None and self._process.poll() is None

    def set_active(self, wanted):
        """Start `caffeinate -i` when a scan runs, stop it when none does. No caffeinate (not macOS): nothing happens."""
        if wanted and not self.active:
            binary = self._which("caffeinate")
            if binary:
                self._process = self._popen(
                    [binary, "-i"],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
        elif not wanted:
            self.stop()

    def stop(self):
        if self._process is not None and self._process.poll() is None:
            self._process.terminate()
        self._process = None
