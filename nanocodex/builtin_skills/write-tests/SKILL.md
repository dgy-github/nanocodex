---
name: write-tests
description: Write focused, meaningful tests for new code or a bug fix — cover behavior and edge cases, not line count.
---

# Write tests

Test observable behavior, not implementation details. A good test fails for a
real reason and reads like a spec.

## Before writing

- Find how the project already tests: the test framework, where tests live, how
  they run. Match that style — don't introduce a new framework.
- Identify what you're testing: the public behavior, the contract, the bug.

## What to cover

- **Happy path**: the normal, expected use returns the right result.
- **Edge cases**: empty input, zero/negative, boundaries, very large input,
  unicode/encoding (especially on Windows — cp1252 vs utf-8).
- **Error paths**: invalid input raises the right error; failures degrade as
  intended rather than crashing.
- **Regression**: for a bug fix, write the test that fails on the old code and
  passes on the fixed code. Confirm it actually fails first.

## How to write them

- One behavior per test; a clear name that says what it asserts.
- Arrange / act / assert. Keep setup minimal and obvious.
- Prefer pure functions and injected dependencies over mocks; mock only true
  boundaries (network, clock, filesystem) when needed.
- Deterministic: no real network, no sleep-based timing, no reliance on wall
  clock. Inject `now`/paths instead.

## After writing

- Run the tests and confirm they pass.
- For a fix, confirm the regression test fails without the fix.
- Don't chase coverage numbers — a passing test that asserts nothing is worse
  than no test.
