"""Pure-function tests for the GUI right-side file-diff panel.

Mirrors tests/test_gui_status.py: exercises the Tk-free helpers in gui.py so
the diff-classification logic is verified without a display or disk.
"""

from __future__ import annotations

from nanocodex.gui import (
    _FILE_PANEL_MAX_ROWS,
    _build_file_edit_payload,
    _line_gutter,
)


def _wrap(*body: str) -> str:
    return "*** Begin Patch\n" + "\n".join(body) + "\n*** End Patch"


def test_add_file_rows_are_all_added_and_numbered():
    patch = _wrap("*** Add File: a.py", "+one", "+two", "+three")
    payload = _build_file_edit_payload(patch)
    assert payload is not None
    (f,) = payload["files"]
    assert f["op"] == "A"
    assert f["path"] == "a.py"
    assert [r["kind"] for r in f["rows"]] == ["added", "added", "added"]
    assert [r["new_no"] for r in f["rows"]] == [1, 2, 3]
    assert [r["text"] for r in f["rows"]] == ["one", "two", "three"]


def test_update_single_chunk_removed_before_added():
    patch = _wrap(
        "*** Update File: a.py",
        "@@ def main():",
        "-    print('hi')",
        "+    print('hello')",
    )
    payload = _build_file_edit_payload(patch)
    assert payload is not None
    (f,) = payload["files"]
    assert f["op"] == "M"
    kinds = [r["kind"] for r in f["rows"]]
    assert kinds == ["hunk_sep", "removed", "added"]
    assert f["rows"][0]["text"] == "def main():"
    assert f["rows"][1]["text"] == "    print('hi')"
    assert f["rows"][2]["text"] == "    print('hello')"


def test_delete_file_is_placeholder_row():
    patch = _wrap("*** Delete File: gone.py")
    payload = _build_file_edit_payload(patch)
    assert payload is not None
    (f,) = payload["files"]
    assert f["op"] == "D"
    assert f["path"] == "gone.py"
    assert [r["text"] for r in f["rows"]] == ["(file deleted)"]


def test_update_with_move_to_is_rename():
    patch = _wrap(
        "*** Update File: old.py",
        "*** Move to: new.py",
        "@@",
        "-a",
        "+b",
    )
    payload = _build_file_edit_payload(patch)
    assert payload is not None
    (f,) = payload["files"]
    assert f["op"] == "R"
    assert f["path"] == "old.py"
    assert f["move_to"] == "new.py"


def test_multi_file_patch_yields_multiple_files():
    patch = _wrap(
        "*** Add File: a.py",
        "+x",
        "*** Delete File: b.py",
    )
    payload = _build_file_edit_payload(patch)
    assert payload is not None
    assert len(payload["files"]) == 2
    assert [f["op"] for f in payload["files"]] == ["A", "D"]


def test_malformed_patch_returns_none():
    assert _build_file_edit_payload("not a patch") is None
    assert _build_file_edit_payload("") is None
    assert _build_file_edit_payload(None) is None  # type: ignore[arg-type]


def test_row_cap_sets_truncated():
    body = ["*** Add File: big.py"] + [f"+line {i}" for i in range(_FILE_PANEL_MAX_ROWS + 50)]
    payload = _build_file_edit_payload(_wrap(*body))
    assert payload is not None
    (f,) = payload["files"]
    assert f["truncated"] is True
    assert len(f["rows"]) == _FILE_PANEL_MAX_ROWS


def test_line_gutter_formats_number_and_blank():
    assert _line_gutter(3) == "   3 "
    assert _line_gutter(None) == " " * 5
    assert _line_gutter(1234) == "1234 "
