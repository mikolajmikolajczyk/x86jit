---
id: TASK-91
title: >-
  INT-T10 —  *(acceptance)* — file-DB sqlite: `sqlite3 <db> < ops.sql` runs
  three
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 91000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
*(acceptance)* — file-DB sqlite: `sqlite3 <db> < ops.sql` runs three ways (`tests/sqlite_file.rs`), creating and mutating a real on-disk database. Added a bounded writable-file passthrough (`allow_write_dir` → `O_RDWR`/`O_CREAT`/`O_TRUNC` under a per-test temp dir; `write`/`pwrite`/`ftruncate`/`fsync`/`unlink`/`lstat` + no-op `chmod`/`chown`), a `stdin` buffer, and the `cbw`/`cwde`/`cdqe` sign-extends. (testing.md §12.5)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
