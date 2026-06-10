"""Offline tests for the storyboard pipeline.

Everything runs with fakes — no network, no real keys, no Seedance spend:
* schema validation (valid + invalid shapes)
* rule-based asset mapping
* Seedance payload assembly
* the full pipeline via fake Vision/Planner/Seedance clients (render off + on)
* SeedanceClient submit/poll parsing via a scripted fake transport
"""

from __future__ import annotations

import base64
import json

import pytest

from nanocodex.storyboard.clients import (
    SeedanceClient,
    SeedanceError,
    SeedanceResult,
    _extract_json,
)
from nanocodex.storyboard.models import (
    AssetAnalysis,
    Shot,
    StoryboardError,
    validate_project,
)
from nanocodex.storyboard.pipeline import (
    PipelineDeps,
    build_payloads,
    ingest,
    map_assets,
    run_pipeline,
)

# A tiny valid PNG (magic bytes + padding) so encode_image_block accepts it.
_PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32


def _valid_obj(image_paths=None):
    images = []
    for i, p in enumerate(image_paths or [], 1):
        images.append({"image_id": f"img_{i:02d}", "path": str(p), "kind": "unknown"})
    return {
        "project": {
            "id": "p1",
            "title": "Test",
            "target_model": "seedance",
            "aspect_ratio": "16:9",
        },
        "inputs": {"story_text": "Once upon a time.", "images": images},
    }


# --- schema validation ------------------------------------------------------


def test_validate_accepts_minimal_valid():
    validate_project(_valid_obj())  # no raise


def test_validate_rejects_missing_title():
    obj = _valid_obj()
    del obj["project"]["title"]
    with pytest.raises(StoryboardError, match="title"):
        validate_project(obj)


def test_validate_rejects_bad_image_kind():
    obj = _valid_obj()
    obj["inputs"]["images"] = [{"image_id": "x", "path": "p", "kind": "banana"}]
    with pytest.raises(StoryboardError):
        validate_project(obj)


# --- _extract_json ----------------------------------------------------------


def test_extract_json_plain():
    assert _extract_json('{"a": 1}') == {"a": 1}


def test_extract_json_fenced():
    assert _extract_json('```json\n{"a": 2}\n```') == {"a": 2}


def test_extract_json_embedded_in_prose():
    assert _extract_json('here you go: {"a": 3} done') == {"a": 3}


def test_extract_json_raises_on_none():
    with pytest.raises(ValueError):
        _extract_json("no json here")


# --- rule-based map_assets --------------------------------------------------


def test_map_assets_splits_character_and_background():
    obj = _valid_obj()
    state = ingest(obj)
    state.images = []  # build images manually below via a fresh state
    # Two images: one declared character, one background.
    from nanocodex.storyboard.models import ImageInput

    state.images = [
        ImageInput(image_id="c1", path="c.png", kind="character"),
        ImageInput(image_id="b1", path="b.png", kind="background"),
    ]
    state.shots = [Shot(shot_id="s1", title="S1", duration_sec=5, prompt="x")]
    map_assets(state)
    assert state.shots[0].character_image_ids == ["c1"]
    assert state.shots[0].background_image_ids == ["b1"]


def test_map_assets_infers_from_vl_tags_when_kind_unknown():
    from nanocodex.storyboard.models import ImageInput

    obj = _valid_obj()
    state = ingest(obj)
    state.images = [
        ImageInput(image_id="a", path="a.png", kind="unknown"),
        ImageInput(image_id="b", path="b.png", kind="unknown"),
    ]
    state.asset_analysis = [
        AssetAnalysis(image_id="a", summary="", usable_for=["character close-up"]),
        AssetAnalysis(image_id="b", summary="", scene_tags=["corridor"]),
    ]
    state.shots = [Shot(shot_id="s1", title="S1", duration_sec=5, prompt="x")]
    map_assets(state)
    assert "a" in state.shots[0].character_image_ids
    assert state.shots[0].background_image_ids == ["b"]


# --- build_payloads ---------------------------------------------------------


def test_build_payloads_shape():
    from nanocodex.storyboard.models import ImageInput

    obj = _valid_obj()
    state = ingest(obj)
    state.images = [ImageInput(image_id="c1", path="/abs/c.png", kind="character")]
    state.shots = [
        Shot(
            shot_id="s1", title="S1", duration_sec=8, prompt="a knight stands",
            negative_prompt="no modern objects", character_image_ids=["c1"],
        )
    ]
    build_payloads(state)
    assert len(state.payloads) == 1
    payload = state.payloads[0].payload
    assert payload["ratio"] == "16:9"
    assert payload["duration"] == 8
    assert payload["watermark"] is False
    # text block carries prompt + negative; reference_image points at the path.
    text_block = payload["content"][0]
    assert text_block["type"] == "text"
    assert "no modern objects" in text_block["text"]
    ref = [c for c in payload["content"] if c.get("role") == "reference_image"]
    assert ref and ref[0]["image_url"]["url"] == "/abs/c.png"


# --- full pipeline with fakes (offline) -------------------------------------


class _FakeVision:
    async def analyze(self, image_id, image_path):
        return AssetAnalysis(image_id=image_id, summary="a thing",
                             usable_for=["background"])


class _FakePlanner:
    async def plan(self, story_text, *, aspect_ratio="16:9", global_style=""):
        return [
            Shot(shot_id="shot_01", title="Open", duration_sec=5, prompt="scene one"),
            Shot(shot_id="shot_02", title="Close", duration_sec=6, prompt="scene two"),
        ]


class _FakeSeedance:
    def generate(self, payload, *, on_progress=None, **kw):
        if on_progress:
            on_progress(0, "succeeded")
        # Mirror the real client: return a SeedanceResult carrying usage so the
        # pipeline can register cost. 108900 is the live-verified 5s/720p count.
        return SeedanceResult(
            video_url="https://example.com/video.mp4?sig=abc",
            usage={"completion_tokens": 108900, "total_tokens": 108900},
        )


async def test_pipeline_offline_no_render(tmp_path):
    p = tmp_path / "a.png"
    p.write_bytes(_PNG)
    obj = _valid_obj([p])
    deps = PipelineDeps(vision=_FakeVision(), planner=_FakePlanner(), seedance=_FakeSeedance())
    state, written = await run_pipeline(obj, deps, out_dir=tmp_path / "out", render_video=False)

    assert len(state.shots) == 2
    assert len(state.asset_analysis) == 1
    assert len(state.payloads) == 2
    assert state.video_urls == {}  # render off -> no spend
    # three JSON files exist and parse.
    for name in ("asset_analysis.json", "storyboard.json", "seedance_payloads.json"):
        data = json.loads((tmp_path / "out" / name).read_text(encoding="utf-8"))
        assert isinstance(data, list)
    assert not (tmp_path / "out" / "video_urls.json").exists()


async def test_pipeline_offline_with_render(tmp_path):
    p = tmp_path / "a.png"
    p.write_bytes(_PNG)
    obj = _valid_obj([p])
    deps = PipelineDeps(vision=_FakeVision(), planner=_FakePlanner(), seedance=_FakeSeedance())
    state, written = await run_pipeline(obj, deps, out_dir=tmp_path / "out", render_video=True)

    assert set(state.video_urls) == {"shot_01", "shot_02"}
    assert all(u.startswith("https://") for u in state.video_urls.values())
    urls_doc = json.loads((tmp_path / "out" / "video_urls.json").read_text(encoding="utf-8"))
    assert "expire" in urls_doc["_note"]


# --- SeedanceClient submit/poll parsing via fake transport ------------------


def _scripted_transport(responses):
    """Return a transport callable that pops (status, body) per call."""
    calls = list(responses)

    def _t(method, url, headers, body):
        return calls.pop(0)
    return _t


def test_seedance_requires_key():
    with pytest.raises(SeedanceError, match="ARK API key"):
        SeedanceClient("")


def test_seedance_generate_happy_path():
    transport = _scripted_transport([
        (200, json.dumps({"id": "task_1"})),                         # submit
        (200, json.dumps({"status": "running"})),                    # poll 1
        (200, json.dumps({"status": "succeeded",
                          "content": {"video_url": "https://v/clip.mp4"},
                          "usage": {"total_tokens": 108900}})),      # poll 2
    ])
    client = SeedanceClient("k", transport=transport, sleep=lambda s: None)
    result = client.generate({"model": "m"}, max_polls=5, interval_s=0)
    # generate now returns a SeedanceResult carrying the URL + billing usage.
    assert result.video_url == "https://v/clip.mp4"
    assert result.usage.get("total_tokens") == 108900


def test_seedance_generate_raises_on_failed():
    transport = _scripted_transport([
        (200, json.dumps({"id": "task_2"})),
        (200, json.dumps({"status": "failed"})),
    ])
    client = SeedanceClient("k", transport=transport, sleep=lambda s: None)
    with pytest.raises(SeedanceError, match="failed"):
        client.generate({"model": "m"}, max_polls=5, interval_s=0)


def test_seedance_submit_http_error():
    transport = _scripted_transport([(400, '{"error": "bad"}')])
    client = SeedanceClient("k", transport=transport, sleep=lambda s: None)
    with pytest.raises(SeedanceError, match="submit failed"):
        client.submit({"model": "m"})
