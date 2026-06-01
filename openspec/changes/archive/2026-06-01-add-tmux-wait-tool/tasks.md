## 1. Specification And Documentation

- [x] 1.1 Update PRD, README, tool spec, and config examples for `tmux_wait`.
- [x] 1.2 Update GitHub issue and PR scope text to include `tmux_wait`.

## 2. Implementation

- [x] 2.1 Add shared tmux event expression parsing and response metadata.
- [x] 2.2 Implement `tmux_wait` for `output_update`, `output_match`,
  `program_exit`, `program_output`, `hook`, and `sleep`.
- [x] 2.3 Register and export `tmux_wait` and add session runner hints.

## 3. Verification

- [x] 3.1 Add focused tests for catalog registration, schema behavior, fake tmux
  event waits, sleep waits, and real tmux round trip coverage.
- [x] 3.2 Run formatting, Rust tests, and strict OpenSpec validation.
- [x] 3.3 Archive the OpenSpec change after implementation validates.
