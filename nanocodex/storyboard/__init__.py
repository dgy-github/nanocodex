"""Storyboard video pipeline: story text + images -> shots -> Seedance video.

A self-contained sub-package that turns a story and a handful of reference
images into a storyboard (a list of shots) and, optionally, real generated
video clips. It reuses two backends already wired into nanocodex:

* **Qwen-VL** (DashScope, OpenAI-compatible) analyses each image — character vs
  background, scene tags, which shots it suits.
* **Seedance** (Volcengine ARK) renders each shot's payload into a video clip.

House style mirrors ``agent/schedule.py`` and ``agent/pricing.py``: the pipeline
stages and helpers are pure functions over data with the network/model IO
injected, so the whole thing unit-tests offline with fake clients.
"""

from __future__ import annotations

from nanocodex.storyboard.models import (
    AssetAnalysis,
    Character,
    ImageInput,
    Project,
    SeedancePayload,
    Shot,
    StoryboardError,
    validate_project,
)

__all__ = [
    "AssetAnalysis",
    "Character",
    "ImageInput",
    "Project",
    "SeedancePayload",
    "Shot",
    "StoryboardError",
    "validate_project",
]
