---
id: TASK-109.1
title: 'P2.0 — make LinuxShim Send over &Vm (Fd Rc->Arc, write_bytes &self)'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 110000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fd table Arc<Mutex>; Memory/Vm::write_bytes &self; handle + write helpers take &Vm; shim_is_send assertion.
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Landed in commit 229b6a4; 240 tests green, clippy clean. First, decision-independent step.
<!-- SECTION:FINAL_SUMMARY:END -->
