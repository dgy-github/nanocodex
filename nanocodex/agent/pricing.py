"""Token cost estimation from DeepSeek's published per-token prices.

The provider returns a real ``usage`` dict per model call (prompt/completion
tokens, plus DeepSeek's cache-hit/miss split). This module turns that usage into
a US-dollar cost using DeepSeek's official price list.

DESIGN
------
* **Pure functions, no I/O.** :func:`cost_usd` is a pure function over
  ``(model, usage)`` so it unit-tests with no network or filesystem, mirroring
  the project's other pure helpers (auto_reasoning, compaction estimate).
* **Cache-aware.** DeepSeek prices a cache HIT input token ~120x cheaper than a
  MISS (pro: $0.003625 vs $0.435 / 1M). When the usage reports the hit/miss
  split we bill each at its own rate; when it doesn't, we conservatively treat
  the whole prompt as a cache MISS (the expensive rate) so we never understate.
* **Honest about staleness.** Prices are a hardcoded snapshot. DeepSeek "reserves
  the right to adjust them", so the table carries its source + as-of date and an
  unknown model returns ``None`` (cost unknown) rather than a wrong number.
* **Refuses to guess.** Some models (e.g. Seedance video) bill on axes a usage
  dict doesn't capture cleanly (video-input mode, resolution/duration, token
  resource packages). Those are listed in ``UNSUPPORTED_PRICING_MODELS`` with a
  reason, so the caller shows "not priced yet" instead of a fabricated number.

Source: DeepSeek official pricing (api-docs.deepseek.com), fetched 2026-06-06.
USD per 1,000,000 tokens.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

# As-of date of the hardcoded price snapshot, surfaced in the UI so a stale
# table is visible rather than silently trusted.
PRICING_AS_OF = "2026-06-06"
PRICING_SOURCE = "api-docs.deepseek.com"


@dataclass(frozen=True)
class ModelPrice:
    """USD per 1,000,000 tokens for one model."""

    input_cache_hit: float
    input_cache_miss: float
    output: float


# Official DeepSeek prices, USD / 1M tokens, as of PRICING_AS_OF.
# deepseek-chat / deepseek-reasoner are documented aliases of v4-flash
# (non-thinking / thinking), deprecating 2026-07-24 — priced the same here.
_PRICES: dict[str, ModelPrice] = {
    "deepseek-v4-pro": ModelPrice(0.003625, 0.435, 0.87),
    "deepseek-v4-flash": ModelPrice(0.0028, 0.14, 0.28),
    "deepseek-chat": ModelPrice(0.0028, 0.14, 0.28),
    "deepseek-reasoner": ModelPrice(0.0028, 0.14, 0.28),
}

_PER_TOKENS = 1_000_000


# Models we deliberately DON'T price yet, with the reason. General extension
# point keyed by the same longest-prefix match as _PRICES. Currently empty:
# Seedance moved to its own CNY pricing below once its billing axis was verified.
UNSUPPORTED_PRICING_MODELS: dict[str, str] = {}


# --- Seedance (Volcengine ARK doubao-seedance) video pricing ----------------
#
# Billing axis VERIFIED against a live task (2026-06-10): a 5s/720p clip's task
# response carried ``usage.total_tokens = 108900`` (and equal completion_tokens,
# no prompt_tokens), so Seedance bills PER TOKEN on the returned total_tokens --
# NOT per-video or per-second. Per the account's package terms: only SUCCESSFUL
# generations are billed (failed requests cost nothing), and input text / image /
# audio are not billed separately -- only the returned total_tokens count.
#
# Two CNY rates apply, by whether the INPUT contains video:
#   * input WITHOUT video: 37 CNY / 1M tokens
#   * input WITH    video: 22 CNY / 1M tokens
# Priced in CNY (the package is billed in CNY); kept separate from the USD text
# table above rather than inventing an FX rate. So cost_usd() returns None for
# Seedance -- callers use seedance_cost_cny() instead.
SEEDANCE_PRICING_AS_OF = "2026-06-10"
SEEDANCE_CNY_PER_M_NO_VIDEO_INPUT = 37.0
SEEDANCE_CNY_PER_M_WITH_VIDEO_INPUT = 22.0


def is_seedance(model: str) -> bool:
    """True if *model* is a Seedance video model (priced in CNY, not USD)."""
    return (model or "").strip().lower().startswith("doubao-seedance")


def seedance_cost_cny(
    usage: dict[str, Any] | None, *, has_video_input: bool = False
) -> float | None:
    """CNY cost of one Seedance task from its response ``usage`` dict.

    Bills ``usage.total_tokens`` at the package rate (37 CNY/1M without video
    input, 22 CNY/1M with). Returns ``None`` when there is no positive
    ``total_tokens`` -- e.g. a failed task, which the package does not bill -- so
    the caller can show "no charge" rather than a misleading ``0.00``.
    """
    if not usage:
        return None
    total = _as_int(usage.get("total_tokens"))
    if total <= 0:
        return None
    rate = (
        SEEDANCE_CNY_PER_M_WITH_VIDEO_INPUT
        if has_video_input
        else SEEDANCE_CNY_PER_M_NO_VIDEO_INPUT
    )
    return total * rate / _PER_TOKENS


def unsupported_reason(model: str) -> str | None:
    """Return why *model* is intentionally not priced, or None if not listed.

    Same matching as :func:`price_for` -- exact name, then longest known prefix
    -- so a dated/suffixed variant (``doubao-seedance-2-0-fast-260128``) still
    resolves to the base ``doubao-seedance`` entry.
    """
    name = (model or "").strip().lower()
    if not name:
        return None
    if name in UNSUPPORTED_PRICING_MODELS:
        return UNSUPPORTED_PRICING_MODELS[name]
    for known in sorted(UNSUPPORTED_PRICING_MODELS, key=len, reverse=True):
        if name.startswith(known):
            return UNSUPPORTED_PRICING_MODELS[known]
    return None


def price_for(model: str) -> ModelPrice | None:
    """Look up the price for *model*, or None if we don't have it.

    Matches the exact name first, then a longest known-prefix (so a dated or
    suffixed variant like ``deepseek-v4-pro-0606`` still prices off the base).
    """
    name = (model or "").strip().lower()
    if not name:
        return None
    if name in _PRICES:
        return _PRICES[name]
    # Longest matching known prefix wins (v4-pro before a hypothetical v4).
    for known in sorted(_PRICES, key=len, reverse=True):
        if name.startswith(known):
            return _PRICES[known]
    return None


def cost_usd(model: str, usage: dict[str, Any] | None) -> float | None:
    """USD cost of one model call from its usage dict.

    Returns ``None`` when the model price is unknown or usage is empty, so the
    caller can show "cost unknown" instead of a misleading ``$0.00``.

    Cache accounting: if usage carries ``prompt_cache_hit_tokens`` /
    ``prompt_cache_miss_tokens`` we bill each input slice at its own rate.
    Otherwise the whole ``prompt_tokens`` count is billed at the (more
    expensive) cache-MISS rate so the estimate never understates the bill.
    """
    price = price_for(model)
    if price is None or not usage:
        return None

    prompt = _as_int(usage.get("prompt_tokens"))
    completion = _as_int(usage.get("completion_tokens"))
    hit = _as_int(usage.get("prompt_cache_hit_tokens"))
    miss = _as_int(usage.get("prompt_cache_miss_tokens"))

    if hit or miss:
        input_cost = (
            hit * price.input_cache_hit + miss * price.input_cache_miss
        )
    else:
        # No cache split reported — treat the whole prompt as a miss.
        input_cost = prompt * price.input_cache_miss

    output_cost = completion * price.output
    return (input_cost + output_cost) / _PER_TOKENS


def add_usage(acc: dict[str, int], usage: dict[str, Any] | None) -> dict[str, int]:
    """Accumulate one usage dict into a running total (pure; returns a new dict).

    Sums prompt/completion and the cache hit/miss fields so a whole turn (or a
    whole session) can be priced as one combined usage. Unknown keys are ignored.
    """
    keys = (
        "prompt_tokens",
        "completion_tokens",
        "prompt_cache_hit_tokens",
        "prompt_cache_miss_tokens",
    )
    out = dict(acc)
    for k in keys:
        added = _as_int(usage.get(k)) if usage else 0
        if added:
            out[k] = out.get(k, 0) + added
    return out


def _as_int(value: Any) -> int:
    try:
        return int(value) if value is not None else 0
    except (TypeError, ValueError):
        return 0
