---
name: debug
description: Systematically find a bug's root cause before changing code — reproduce, localize, fix, verify.
---

# Debug

Find the root cause before touching code. Resist the urge to patch the first
plausible-looking line.

## 1. Reproduce

- Get an exact, repeatable trigger. If you can't reproduce it, you can't confirm
  a fix — say so and ask for the steps/input that trigger it.
- Capture the real error: full traceback, exit code, or the wrong output vs the
  expected output. Read it; don't guess from the summary.

## 2. Localize

- State your leading hypothesis in one sentence, and what evidence supports it.
- Narrow the search: which function/file owns the failing behavior? Read it.
- Add a targeted observation (print/log/breakpoint, or a failing unit test) that
  confirms or kills the hypothesis. One high-value check beats five guesses.
- If the check kills the hypothesis, form a new one. Don't keep patching.

## 3. Fix

- Change the smallest thing that addresses the root cause, not the symptom.
- If a symptom and its cause are in different places, fix the cause.

## 4. Verify

- Re-run the original repro: the bug is gone.
- Run the surrounding tests: nothing else broke.
- Add a regression test that fails before the fix and passes after, when the
  project has a test setup.

## Rules

- After two failed attempts on the same approach, stop and reconsider the root
  cause instead of making more incremental tweaks.
- Label facts (observed output, file contents) separately from inferences
  (likely cause). Don't promote a guess to a conclusion.
