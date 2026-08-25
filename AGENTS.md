# Repository Guidelines

## Project Overview

Ports is a macOS-first Rust CLI and Ratatui TUI for answering which local ports and sockets are active, which processes own them, and what actions are safe to take. It is designed for ordinary-user developer workflows; it is not a packet sniffer, port scanner, firewall, or general process monitor.

## Architecture & Data Flow

- `src/discovery.rs` is the platform boundary. It invokes `/usr/sbin/lsof` in field mode, parses TCP/UDP IPv4/IPv6 records, enriches process data with `ps` and bounded cwd lookups, then correlates sockets into `ServiceRecord` rows. Enrichment is best effort; malformed or permission-hidden metadata must not discard otherwise useful sockets.
- `src/model.rs` contains the serializable domain model: protocols, socket states, network exposure scopes, process metadata, endpoints, services, sockets, and connections.
- `src/filter.rs` defines reusable conjunctive filters. Criteria match service fields and relevant peer connections; free-text search is case-insensitive, substring-based, and whitespace-token AND matching.
- `src/main.rs` parses Clap arguments. No subcommand launches the TUI; subcommands dispatch the CLI.
- `src/cli.rs` consumes the shared discovery/model/filter APIs for list, inspect, process, connections, and kill commands. Read commands rediscover state per invocation; kill operations require an unambiguous owner and explicit confirmation unless `--yes` is provided.
- `src/app.rs` owns mutable TUI state: services, filtered indices, stable selection identity, overlays, status/error state, refresh diffs, and bounded activity history. Discovery refreshes every 900 ms; selection is preserved by protocol/address/port/PID identity.
- `src/tui.rs` owns terminal lifecycle, the 80 ms input loop, Ratatui rendering, and responsive layouts. `TerminalGuard` restores raw mode and the alternate screen on drop.

The backend is synchronous and macOS-coupled. Discovery uses blocking `std::process::Command`; there is no async backend or injected discovery interface. Keep parser/grouping semantics stable because both CLI and TUI depend on them.

## Key Directories

- `src/` — library model/filter/discovery code plus binary-only CLI and TUI modules.
- `tests/` — integration suites for discovery, model/filter behavior, and CLI parsing/output helpers.
- `.github/workflows/` — tagged macOS release automation.
- `Formula/` — Homebrew formula template rendered with release-specific checksums.
- `Cargo.toml`, `Cargo.lock` — package metadata and locked dependencies.
- `README.md` — user-facing installation, command, keybinding, and release documentation.

## Development Commands

Run from the repository root:

```sh
cargo fmt --all -- --check       # formatting gate
cargo check --all-targets        # compile library, binary, and tests
cargo test --all-targets         # full unit and integration suite
cargo test --test discovery      # focused discovery parser/correlation tests
cargo test --test model          # focused model/filter tests
cargo test --test cli            # focused CLI tests
cargo test <test_name>           # one matching test
cargo run -- --help              # CLI help
cargo run -- list --json         # live JSON discovery
cargo run                         # launch the TUI in a real terminal
```

No lint script or CI lint gate is configured. Use `cargo clippy --all-targets` for optional local linting. Format Rust changes with rustfmt; do not hand-format around its output.

Tagged releases use `vX.Y.Z`. `.github/workflows/release.yml` builds stable Rust artifacts for `aarch64-apple-darwin` and `x86_64-apple-darwin`, publishes checksums/GitHub assets, and updates `noahlin34/homebrew-tap` using `HOMEBREW_TAP_TOKEN`.

## Code Conventions & Common Patterns

- Follow idiomatic Rust naming and module boundaries. Keep shared, serializable concepts in `src/model.rs`; expose only `discovery`, `filter`, and `model` from `src/lib.rs`.
- Prefer typed `Endpoint`/enum fields over encoded strings. Preserve `serde` derives and stable JSON shapes when changing model records.
- Use `anyhow::Result`/context for discovery and runtime failures; use the structured `CliError` path for user-facing CLI failures. Surface permission and missing-metadata failures as useful status/details rather than panicking.
- Keep platform-specific calls behind `cfg(target_os = "macos")` and preserve non-macOS unsupported behavior. Do not assume root access.
- Keep filtering composable and AND-based. Add criteria to the shared `Filter` when both CLI and TUI need the behavior; avoid duplicating matching logic in front ends.
- Keep TUI mutations in `App`; rendering in `tui.rs` should remain a pure view of state. Preserve stable selection through refreshes, bounded history retention, and overlay key trapping.
- Destructive process actions must show the exact port/process/PID and require confirmation. Validate PIDs before sending signals.
- Discovery and metadata enrichment are blocking and best effort. Do not introduce per-row unbounded subprocesses or silently replace real system data with fixtures.
- Integration tests intentionally use public APIs; `tests/cli.rs` imports `src/cli.rs` directly with `#[path = "../src/cli.rs"]`, so CLI module layout changes require updating that test coupling.

## Important Files

- `src/main.rs` — executable entry point and command dispatch.
- `src/discovery.rs` — macOS `lsof` parser, process enrichment, socket correlation, signal handling.
- `src/model.rs` — core records and network-scope classification.
- `src/filter.rs` — shared service/socket/connection filtering.
- `src/cli.rs` — Clap commands, human/JSON output, kill safety.
- `src/app.rs` — live TUI state, refresh/history/selection/actions.
- `src/tui.rs` — terminal setup, event loop, layouts, rendering.
- `src/theme.rs`, `src/help.rs` — visual palette and keyboard-help surface.
- `tests/discovery.rs`, `tests/model.rs`, `tests/cli.rs` — integration coverage and in-memory fixtures.
- `.github/workflows/release.yml`, `Formula/ports.rb` — release and Homebrew publication.

## Runtime/Tooling Preferences

Use stable Rust with Cargo, Rust 2021, and the minimum supported toolchain declared by `rust-version = "1.82"`. Cargo is the package manager and build/test runner. There is no Node/Bun/Python package workflow, Makefile, Taskfile, justfile, `build.rs`, or checked-in toolchain file.

Runtime behavior is macOS-specific: `/usr/sbin/lsof`, `ps`, and libc signal APIs provide discovery and process actions. Normal operation should work without sudo, though system-owned processes may expose incomplete metadata or reject termination. Release artifacts target macOS arm64 and Intel only.

## Testing & QA

Tests are standard Cargo tests with no dev-dependencies or custom test configuration:

- `tests/discovery.rs` uses inline NUL/newline `lsof` fixtures for TCP/UDP, IPv4/IPv6, wildcard listeners, peers, duplicates, malformed records, permission gaps, and correlation.
- `tests/model.rs` covers network-scope boundaries, Tailscale classification, composed filters, endpoint display, and JSON serialization.
- `tests/cli.rs` covers Clap parsing, listener/all/current-user filters, narrow table rendering, and kill-target ambiguity/safety.
- Source-local tests in `src/app.rs`, `src/tui.rs`, and `src/help.rs` cover selection/history/overlay state, Ratatui `TestBackend` rendering at wide and narrow sizes, and help layout.

Use focused suites while iterating, then run `cargo test --all-targets`. Coverage is deterministic logic and renderer smoke coverage; there are no end-to-end tests for real `lsof`/`ps` subprocesses, terminal event loops, JSON command output, signal delivery, or macOS permission behavior. Changes to those boundaries require manual smoke testing on macOS in addition to unit/integration tests.
