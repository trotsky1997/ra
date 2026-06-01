# Add Extended Built-In Tools

## Why

Ra currently exposes the basic file and shell tools plus native `git` and
`gh`. Common agent workflows still route search, file discovery, directory
listing, fuzzy filtering, and patch application through `bash`, which loses
structured parameters, predictable output, and narrow permission semantics.

## What Changes

- Add built-in extended tools: `grep`, `glob`, `ls`, `fuzzy`, and
  `apply_patch`.
- Keep `bash` as the general fallback while making high-frequency read-only
  operations structured and gitignore-aware by default.
- Implement `apply_patch` as a controlled `git apply` wrapper that checks
  patches before applying and participates in existing file approval.
- Update tool registration, UI tool hints/titles, init templates, README, and
  tool reference docs.

## Non-Goals

- Do not expose the full `git apply` option surface.
- Do not implement an interactive TTY `fzf` UI.
- Do not remove or restrict `bash`.
