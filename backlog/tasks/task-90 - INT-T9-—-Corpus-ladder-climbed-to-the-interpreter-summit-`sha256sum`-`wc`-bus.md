---
id: TASK-90
title: >-
  INT-T9 — Corpus ladder climbed to the interpreter summit: `sha256sum`/`wc`
  (bus
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:tests'
milestone: open-backlog
dependencies: []
ordinal: 90000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Corpus ladder climbed to the interpreter summit: `sha256sum`/`wc` (busybox), musl `sha256sum`, **`sqlite3`** (in-memory query), **`lua`** (x87), and **`CPython 3.13`** (`python3 -S -c`, full bytecode VM). Also dynamically-linked musl + glibc hellos. **Further rungs done:** file-DB sqlite (`tests/sqlite_file.rs`), **gzip/gunzip** (DEFLATE, `tests/gzip.rs`), **libjpeg-turbo `djpeg`** (JPEG decode, real SSE2/SSSE3 codec DSP — `tests/djpeg.rs`). **Still open:** a larger python script (more stdlib), `git`/`perl`/`node`, `cjpeg` (encode).
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
