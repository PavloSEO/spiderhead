"""SEOHEAD native desktop presentation skeleton."""

try:  # every entry point and test imports this package first: Python must never tear Qt objects down at exit
    from PyQt5 import sip

    sip.setdestroyonexit(False)  # otherwise dealloc_QApplication crashes the interpreter on exit (SIGSEGV)
except ImportError:  # PyQt5 absent: nothing to protect (headless tooling)
    pass
