"""Tests for token cost estimation (agent/pricing.py)."""

from __future__ import annotations

from nanocodex.agent.pricing import (
    add_usage,
    cost_usd,
    price_for,
)


def test_price_for_exact_and_prefix_and_unknown():
    assert price_for("deepseek-v4-pro") is not None
    # A dated/suffixed variant prices off the base name via prefix match.
    assert price_for("deepseek-v4-pro-0606") is price_for("deepseek-v4-pro")
    # Unknown model -> None (cost unknown, never a wrong number).
    assert price_for("gpt-4o") is None
    assert price_for("") is None


def test_cost_unknown_model_returns_none():
    assert cost_usd("mystery-model", {"prompt_tokens": 100}) is None


def test_cost_empty_usage_returns_none():
    assert cost_usd("deepseek-v4-pro", {}) is None
    assert cost_usd("deepseek-v4-pro", None) is None


def test_cost_with_cache_split_bills_each_rate():
    # pro: hit $0.003625, miss $0.435, output $0.87 per 1M tokens.
    # 1M hit + 1M miss input, 1M output =
    #   0.003625 + 0.435 + 0.87 = 1.308625 USD
    usage = {
        "prompt_tokens": 2_000_000,
        "prompt_cache_hit_tokens": 1_000_000,
        "prompt_cache_miss_tokens": 1_000_000,
        "completion_tokens": 1_000_000,
    }
    cost = cost_usd("deepseek-v4-pro", usage)
    assert cost is not None
    assert abs(cost - 1.308625) < 1e-9


def test_cost_without_cache_split_treats_all_as_miss():
    # No hit/miss fields: whole prompt billed at the (expensive) miss rate.
    # 1M prompt @ miss 0.435 + 1M output @ 0.87 = 1.305 USD
    usage = {"prompt_tokens": 1_000_000, "completion_tokens": 1_000_000}
    cost = cost_usd("deepseek-v4-pro", usage)
    assert cost is not None
    assert abs(cost - 1.305) < 1e-9


def test_cost_all_cache_hit_is_far_cheaper_than_miss():
    hit_usage = {
        "prompt_tokens": 1_000_000,
        "prompt_cache_hit_tokens": 1_000_000,
        "prompt_cache_miss_tokens": 0,
    }
    miss_usage = {
        "prompt_tokens": 1_000_000,
        "prompt_cache_hit_tokens": 0,
        "prompt_cache_miss_tokens": 1_000_000,
    }
    hit = cost_usd("deepseek-v4-pro", hit_usage)
    miss = cost_usd("deepseek-v4-pro", miss_usage)
    assert hit is not None and miss is not None
    # Cache hit input is ~120x cheaper than a miss.
    assert miss > hit * 100


def test_flash_cheaper_than_pro():
    usage = {"prompt_tokens": 1_000_000, "completion_tokens": 1_000_000}
    pro = cost_usd("deepseek-v4-pro", usage)
    flash = cost_usd("deepseek-v4-flash", usage)
    assert pro is not None and flash is not None
    assert flash < pro


def test_add_usage_accumulates_all_fields():
    acc = {}
    acc = add_usage(acc, {
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "prompt_cache_hit_tokens": 30,
        "prompt_cache_miss_tokens": 70,
    })
    acc = add_usage(acc, {
        "prompt_tokens": 10,
        "completion_tokens": 5,
        "prompt_cache_hit_tokens": 3,
        "prompt_cache_miss_tokens": 7,
    })
    assert acc["prompt_tokens"] == 110
    assert acc["completion_tokens"] == 55
    assert acc["prompt_cache_hit_tokens"] == 33
    assert acc["prompt_cache_miss_tokens"] == 77


def test_add_usage_is_pure_and_handles_none():
    acc = {"prompt_tokens": 5}
    out = add_usage(acc, None)
    assert out == {"prompt_tokens": 5}
    assert out is not acc  # returns a new dict, doesn't mutate input


def test_add_usage_then_cost_round_trip():
    # Accumulate two calls' usage, then price the combined total.
    acc = {}
    acc = add_usage(acc, {"prompt_tokens": 500_000, "completion_tokens": 0,
                          "prompt_cache_miss_tokens": 500_000})
    acc = add_usage(acc, {"prompt_tokens": 500_000, "completion_tokens": 0,
                          "prompt_cache_miss_tokens": 500_000})
    # 1M miss input @ 0.435 = 0.435 USD
    cost = cost_usd("deepseek-v4-pro", acc)
    assert cost is not None
    assert abs(cost - 0.435) < 1e-9
