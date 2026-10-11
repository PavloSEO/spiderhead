import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QFrame, QPushButton, QToolButton

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.modal import ModalDialog


class ModalDialogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def test_header_carries_title_subtitle_and_tone(self):
        dialog = ModalDialog(
            "Остановить скан?", "Перепроверка списком", icon="stop_circle", tone="warning"
        )
        self.assertEqual(dialog.title.text(), "Остановить скан?")
        self.assertEqual(dialog.subtitle.text(), "Перепроверка списком")
        self.assertEqual(dialog.findChild(QFrame, "modalIcon").property("tone"), "warning")

    def test_empty_subtitle_is_hidden(self):
        dialog = ModalDialog("Удалить вид?")
        self.assertFalse(dialog.subtitle.isVisibleTo(dialog))

    def test_footer_keeps_caller_order_and_primary_is_default(self):
        dialog = ModalDialog("Закрыть задачи", "Только с подтверждением")
        dialog.add_button("Отмена", role="text", on_click=dialog.reject)
        dialog.add_button("Сохранить как профиль", role="secondary")
        primary = dialog.add_button("Закрыть задачи", role="primary", icon="task_alt")
        labels = [b.text() for b in dialog.findChildren(QPushButton)][-3:]
        self.assertEqual(labels, ["Отмена", "Сохранить как профиль", "Закрыть задачи"])
        self.assertTrue(primary.isDefault())
        self.assertEqual(primary.property("role"), "primary")
        self.assertFalse(primary.icon().isNull())

    def test_secondary_has_no_role_and_destructive_is_filled_role(self):
        dialog = ModalDialog("Удалить вид?")
        plain = dialog.add_button("Отмена", role="secondary")
        destructive = dialog.add_button("Удалить", role="destructive")
        self.assertFalse(plain.property("role"))
        self.assertEqual(destructive.property("role"), "destructive")

    def test_unknown_role_is_rejected(self):
        with self.assertRaises(ValueError):
            ModalDialog("X").add_button("Y", role="bogus")

    def test_button_click_runs_callback_and_close_rejects(self):
        seen = []
        dialog = ModalDialog("Подключение агента")
        dialog.add_button("Готово", role="primary", on_click=lambda: seen.append("done"))
        dialog.findChildren(QPushButton)[-1].click()
        self.assertEqual(seen, ["done"])
        closed = []
        dialog.finished.connect(closed.append)
        dialog.reject()
        self.assertEqual(closed, [0])

    def test_hint_shows_only_when_text_is_set(self):
        dialog = ModalDialog("Новый проект")
        self.assertFalse(dialog.hint.isVisibleTo(dialog))
        dialog.set_hint("Папка пустая — будет создан project.json")
        self.assertEqual(dialog.hint.text(), "Папка пустая — будет создан project.json")
        self.assertTrue(dialog.hint.isVisibleTo(dialog))

    def test_close_button_is_named_for_assistive_tech(self):
        dialog = ModalDialog("О программе")
        named = [w for w in dialog.findChildren(QToolButton) if w.accessibleName() == "Закрыть"]
        self.assertEqual(len(named), 1)


if __name__ == "__main__":
    unittest.main()
