"""Tests for the V4A apply_patch parser and applier."""

from __future__ import annotations

import pytest

from nanocodex.tools.patch import ActionType, PatchError, apply_patch, parse_patch


def _allow_all(_path) -> bool:
    return True


def test_parse_add_file():
    patch = (
        "*** Begin Patch\n"
        "*** Add File: hello.py\n"
        "+print('hi')\n"
        "+print('bye')\n"
        "*** End Patch"
    )
    actions = parse_patch(patch)
    assert len(actions) == 1
    assert actions[0].type is ActionType.ADD
    assert actions[0].path == "hello.py"
    assert actions[0].new_lines == ["print('hi')", "print('bye')"]


def test_apply_add_file(tmp_path):
    patch = (
        "*** Begin Patch\n"
        "*** Add File: pkg/new.py\n"
        "+a = 1\n"
        "*** End Patch"
    )
    outcome = apply_patch(patch, root=tmp_path, can_write=_allow_all)
    created = tmp_path / "pkg" / "new.py"
    assert created.read_text() == "a = 1\n"
    assert outcome.added == ["pkg/new.py"]


def test_apply_add_writes_lf_not_crlf(tmp_path):
    # Regression: write_text must not translate '\n' to '\r\n' on Windows.
    patch = (
        "*** Begin Patch\n"
        "*** Add File: nl.py\n"
        "+line1\n"
        "+line2\n"
        "*** End Patch"
    )
    apply_patch(patch, root=tmp_path, can_write=_allow_all)
    raw = (tmp_path / "nl.py").read_bytes()
    assert b"\r\n" not in raw
    assert raw == b"line1\nline2\n"


def test_apply_add_existing_file_fails(tmp_path):
    (tmp_path / "x.py").write_text("already here\n")
    patch = "*** Begin Patch\n*** Add File: x.py\n+nope\n*** End Patch"
    with pytest.raises(PatchError, match="already exists"):
        apply_patch(patch, root=tmp_path, can_write=_allow_all)


def test_apply_update_simple(tmp_path):
    target = tmp_path / "app.py"
    target.write_text("def main():\n    print('hi')\n    return 0\n")
    patch = (
        "*** Begin Patch\n"
        "*** Update File: app.py\n"
        "-    print('hi')\n"
        "+    print('hello')\n"
        "*** End Patch"
    )
    outcome = apply_patch(patch, root=tmp_path, can_write=_allow_all)
    assert target.read_text() == "def main():\n    print('hello')\n    return 0\n"
    assert outcome.updated == ["app.py"]


def test_apply_update_with_locator(tmp_path):
    target = tmp_path / "app.py"
    target.write_text(
        "def a():\n    x = 1\n    return x\n\n"
        "def b():\n    x = 1\n    return x\n"
    )
    # Use @@ to target the x=1 inside b(), not a().
    patch = (
        "*** Begin Patch\n"
        "*** Update File: app.py\n"
        "@@ def b():\n"
        "-    x = 1\n"
        "+    x = 2\n"
        "*** End Patch"
    )
    apply_patch(patch, root=tmp_path, can_write=_allow_all)
    assert target.read_text() == (
        "def a():\n    x = 1\n    return x\n\n"
        "def b():\n    x = 2\n    return x\n"
    )


def test_apply_update_indentation_fallback(tmp_path):
    # The model provides slightly different leading whitespace; the matcher
    # should still locate the line via the strip() fallback.
    target = tmp_path / "app.py"
    target.write_text("class C:\n        value = 1\n")
    patch = (
        "*** Begin Patch\n"
        "*** Update File: app.py\n"
        "-value = 1\n"
        "+value = 2\n"
        "*** End Patch"
    )
    apply_patch(patch, root=tmp_path, can_write=_allow_all)
    assert "value = 2" in target.read_text()


def test_apply_update_missing_lines_fails_atomically(tmp_path):
    target = tmp_path / "app.py"
    original = "line one\nline two\n"
    target.write_text(original)
    patch = (
        "*** Begin Patch\n"
        "*** Update File: app.py\n"
        "-nonexistent line\n"
        "+replacement\n"
        "*** End Patch"
    )
    with pytest.raises(PatchError, match="could not locate"):
        apply_patch(patch, root=tmp_path, can_write=_allow_all)
    # Atomic: file is untouched.
    assert target.read_text() == original


def test_apply_delete_file(tmp_path):
    target = tmp_path / "gone.py"
    target.write_text("delete me\n")
    patch = "*** Begin Patch\n*** Delete File: gone.py\n*** End Patch"
    outcome = apply_patch(patch, root=tmp_path, can_write=_allow_all)
    assert not target.exists()
    assert outcome.deleted == ["gone.py"]


def test_apply_move_file(tmp_path):
    src = tmp_path / "old.py"
    src.write_text("x = 1\n")
    patch = (
        "*** Begin Patch\n"
        "*** Update File: old.py\n"
        "*** Move to: new.py\n"
        "-x = 1\n"
        "+x = 2\n"
        "*** End Patch"
    )
    outcome = apply_patch(patch, root=tmp_path, can_write=_allow_all)
    assert not src.exists()
    assert (tmp_path / "new.py").read_text() == "x = 2\n"
    assert outcome.moved == [("old.py", "new.py")]


def test_patch_must_have_envelope():
    with pytest.raises(PatchError, match="Begin Patch"):
        parse_patch("*** Add File: x\n+y\n")


def test_apply_respects_can_write(tmp_path):
    patch = "*** Begin Patch\n*** Add File: secret.py\n+x\n*** End Patch"

    def deny(_path) -> bool:
        return False

    with pytest.raises(PatchError, match="outside the writable sandbox"):
        apply_patch(patch, root=tmp_path, can_write=deny)
    assert not (tmp_path / "secret.py").exists()


def test_multiple_actions_in_one_patch(tmp_path):
    (tmp_path / "keep.py").write_text("v = 1\n")
    patch = (
        "*** Begin Patch\n"
        "*** Add File: a.py\n"
        "+a = 1\n"
        "*** Update File: keep.py\n"
        "-v = 1\n"
        "+v = 2\n"
        "*** End Patch"
    )
    outcome = apply_patch(patch, root=tmp_path, can_write=_allow_all)
    assert (tmp_path / "a.py").exists()
    assert (tmp_path / "keep.py").read_text() == "v = 2\n"
    assert outcome.added == ["a.py"]
    assert outcome.updated == ["keep.py"]
