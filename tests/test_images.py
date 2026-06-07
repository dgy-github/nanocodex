"""Tests for image input passthrough (offline; tiny synthetic images)."""

from __future__ import annotations

import base64
from pathlib import Path

import pytest

from nanocodex.agent.images import (
    ImageError,
    build_user_content,
    detect_mime,
    encode_image_block,
)

# Minimal valid-ish byte payloads carrying the right magic signatures.
_PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32
_JPEG = b"\xff\xd8\xff\xe0" + b"\x00" * 32
_GIF = b"GIF89a" + b"\x00" * 32


def _write(tmp_path: Path, name: str, data: bytes) -> Path:
    p = tmp_path / name
    p.write_bytes(data)
    return p


def test_detect_mime_by_magic(tmp_path):
    assert detect_mime(_PNG, tmp_path / "x.bin") == "image/png"
    assert detect_mime(_JPEG, tmp_path / "x.bin") == "image/jpeg"
    assert detect_mime(_GIF, tmp_path / "x.bin") == "image/gif"


def test_detect_mime_extension_fallback(tmp_path):
    # No magic match, but a .png extension -> png.
    assert detect_mime(b"random-bytes", tmp_path / "pic.png") == "image/png"


def test_detect_mime_rejects_unknown(tmp_path):
    with pytest.raises(ImageError):
        detect_mime(b"random-bytes", tmp_path / "pic.bin")


def test_encode_image_block_shape(tmp_path):
    p = _write(tmp_path, "a.png", _PNG)
    block = encode_image_block(p)
    assert block["type"] == "image_url"
    url = block["image_url"]["url"]
    assert url.startswith("data:image/png;base64,")
    # The base64 payload decodes back to the original bytes.
    b64 = url.split(",", 1)[1]
    assert base64.b64decode(b64) == _PNG


def test_encode_missing_file(tmp_path):
    with pytest.raises(ImageError, match="not found"):
        encode_image_block(tmp_path / "nope.png")


def test_encode_empty_file(tmp_path):
    p = _write(tmp_path, "empty.png", b"")
    with pytest.raises(ImageError, match="empty"):
        encode_image_block(p)


def test_build_user_content_text_only_returns_string():
    out = build_user_content("just text", None)
    assert out == "just text"
    out2 = build_user_content("just text", [])
    assert out2 == "just text"


def test_build_user_content_with_images_returns_blocks(tmp_path):
    p1 = _write(tmp_path, "a.png", _PNG)
    p2 = _write(tmp_path, "b.jpg", _JPEG)
    out = build_user_content("look at these", [str(p1), str(p2)])
    assert isinstance(out, list)
    assert out[0] == {"type": "text", "text": "look at these"}
    assert out[1]["type"] == "image_url"
    assert out[2]["type"] == "image_url"
    assert out[1]["image_url"]["url"].startswith("data:image/png;base64,")
    assert out[2]["image_url"]["url"].startswith("data:image/jpeg;base64,")
