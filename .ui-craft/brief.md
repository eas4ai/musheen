# Musheen UI Brief

## 6. Learned constraints

- **2026-09-22** — Wrap stable GPUI entities in `Entity::cached` when their rendered output does not depend on per-frame state, and invalidate the cache whenever an input changes. *Why:* GPUI's immediate-mode rendering revisits the view every frame; caching truly stable subtrees prevents unnecessary CPU usage without allowing stale UI.
