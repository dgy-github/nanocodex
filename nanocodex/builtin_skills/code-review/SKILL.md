---
name: code-review
description: Review changed code for correctness bugs, then reuse/simplification/efficiency cleanups.
---

# Code review

Review the current diff (or the files the user points at) in two passes. Report
findings; do not edit unless the user asks.

## Pass 1 — correctness (highest value)

Look for bugs that change behavior:

- Off-by-one, wrong boundary, inverted condition.
- Unhandled error / exception path; resource left open (file, socket, lock).
- Mutation of shared state; missing `await`; race between concurrent paths.
- Wrong type coercion; `None`/null not handled; empty-collection edge case.
- Security: unvalidated input reaching a query/shell/path; secret logged.

For each finding give: file_path:line_number, what breaks, and a concrete fix.

## Pass 2 — cleanup (only after correctness)

- Duplicated logic that already exists elsewhere — reuse it.
- A simpler equivalent (fewer branches, a stdlib call, an early return).
- Obvious inefficiency in a hot path (repeated work, N+1, needless copy).

## Rules

- Read the code before claiming anything. Quote the line you mean.
- Separate "this is a bug" from "this is a style preference."
- Rank by impact. A correctness bug outranks every cleanup.
- If the diff is clean, say so plainly rather than inventing nits.
