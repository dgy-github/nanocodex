"""Image input: build OpenAI multimodal content blocks from local files.

Honesty note on model support
------------------------------
Attaching an image only helps if the *model* can see it. Vision is a model
capability, not a transport one: this module produces the standard OpenAI
``image_url`` data-URL blocks, but a text-only model (the configured
``deepseek-v4-pro`` is most likely text/reasoning-only — unverified) will ignore
or reject them. The CLI surfaces this caveat when ``--image`` is used. The
passthrough is built so that the day a vision-capable model/endpoint is
configured, images work with no further code changes.
"""

from __future__ import annotations

import base64
import mimetypes
from pathlib import Path
from typing import Any

# Cap a single image so we don't blow the request size (base64 inflates ~33%).
_MAX_IMAGE_BYTES = 8 * 1024 * 1024  # 8 MiB on disk

# Magic-byte signatures for MIME fallback when the extension is missing/wrong.
_MAGIC: list[tuple[bytes, str]] = [
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
    (b"RIFF", "image/webp"),  # RIFF....WEBP; good enough for our purposes
    (b"BM", "image/bmp"),
]


class ImageError(ValueError):
    """Raised when an image cannot be read or is unsupported."""


def detect_mime(raw: bytes, path: Path) -> str:
    """Detect image MIME from magic bytes, falling back to the extension."""
    for sig, mime in _MAGIC:
        if raw.startswith(sig):
            return mime
    guessed = mimetypes.guess_type(str(path))[0]
    if guessed and guessed.startswith("image/"):
        return guessed
    raise ImageError(f"{path.name}: not a recognized image format")


def encode_image_block(path: str | Path) -> dict[str, Any]:
    """Return a single OpenAI ``image_url`` content block for *path*."""
    p = Path(path)
    if not p.is_file():
        raise ImageError(f"image not found: {path}")
    try:
        raw = p.read_bytes()
    except OSError as exc:
        raise ImageError(f"cannot read {path}: {exc}") from exc
    if not raw:
        raise ImageError(f"empty image file: {path}")
    if len(raw) > _MAX_IMAGE_BYTES:
        raise ImageError(
            f"{p.name}: image is {len(raw) / 1024 / 1024:.1f} MiB; "
            f"max is {_MAX_IMAGE_BYTES // 1024 // 1024} MiB"
        )
    mime = detect_mime(raw, p)
    b64 = base64.b64encode(raw).decode("ascii")
    return {
        "type": "image_url",
        "image_url": {"url": f"data:{mime};base64,{b64}"},
    }


def build_user_content(text: str, image_paths: list[str] | None) -> "str | list[dict[str, Any]]":
    """Build a user message's content.

    With no images, returns the plain text string (cheapest, most compatible).
    With images, returns a multimodal block list: the text block first, then one
    ``image_url`` block per image.
    """
    if not image_paths:
        return text
    blocks: list[dict[str, Any]] = [{"type": "text", "text": text}]
    for path in image_paths:
        blocks.append(encode_image_block(path))
    return blocks
