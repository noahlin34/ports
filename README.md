# Ports

Ports is a macOS-first terminal tool for discovering listening ports, network connections, and the processes that own them. Running `ports` without a command opens the interactive TUI; the same discovery and filtering model powers the CLI and JSON output.

## Install

### Homebrew (recommended)

The release workflow publishes a formula to the `noahlin34/homebrew-tap` tap:

```sh
brew tap noahlin34/tap
brew install noahlin34/tap/ports
```

### Release binary

Releases support macOS on Apple silicon (`arm64`) and Intel (`x86_64`). Download the matching archive and install the single executable in a user-owned directory:

```sh
case "$(uname -m)" in
  arm64) asset=ports-macos-arm64.tar.gz ;;
  x86_64) asset=ports-macos-x86_64.tar.gz ;;
  *) echo "Ports releases support macOS arm64 and x86_64 only" >&2; exit 1 ;;
esac

tmpdir="$(mktemp -d)"
curl --fail --location --output "${tmpdir}/ports.tar.gz" \
  "https://github.com/noahlin34/ports/releases/latest/download/${asset}"
tar --extract --gzip --file "${tmpdir}/ports.tar.gz" --directory "${tmpdir}"
install -d "${HOME}/.local/bin"
install -m 0755 "${tmpdir}/ports" "${HOME}/.local/bin/ports"
rm -rf "${tmpdir}"
```

Ensure `~/.local/bin` is on your `PATH`, then check the installation:

```sh
ports --version
```

## CLI usage

With no subcommand, launch the TUI:

```sh
ports
```

Read-only commands are available for scripts and quick inspection:

```sh
ports list
ports inspect 8080
ports process 1234
ports connections
ports kill 8080
ports list --protocol tcp --state listening --scope local
ports connections --user "$USER" --json
ports inspect 8080 --json
```

`kill <port>` terminates the process listening on a port after confirmation. Use `ports --help` for command-specific options. Read commands share these filters: `--protocol`, `--state`, `--scope`, `--process`, `--pid`, `--user`, and `--all`. `--json` is supported by read commands for machine-readable output.

## TUI interaction

Keyboard bindings remain available with mouse support:

| Key | Action |
| --- | --- |
| Arrow keys / `j` `k` | Navigate |
| `Page Up` / `Page Down` | Move one page |
| `Home` / `End` | First / last service |
| `Tab` | Switch view/detail focus |
| `Enter` | Toggle the selected detail view |
| `/` | Search services |
| `p` | Show the full executable path |
| `r` | Refresh discovery |
| `c` | Copy the raw bind endpoint |
| `u` | Copy a conservative local HTTP URL |
| `o` | Open a likely HTTP service |
| `x` | Terminate with SIGTERM confirmation |
| `X` | Force-kill with SIGKILL; type `KILL` to confirm |
| `?` | Show help |
| `Esc` | Close help or the active overlay |
| `q` | Quit |

### Mouse

| Mouse | Action |
| --- | --- |
| Move pointer | Highlight rows, panels, and actions |
| Single-click a row | Select the row and focus Overview |
| Single-click a panel | Focus that panel |
| Wheel up/down over Overview | Move selection one row (same as Arrow up/down) |
| Single-click a visible action | Trigger its keyboard equivalent |
| Overlay open | Modal controls; outside clicks no-op |

When shown, footer actions (`/ search`, `Tab focus`, `p path`, `x kill`, `? help`, and `q quit`) and modal controls (`Enter` to apply, `Esc` to cancel or close, and confirmation buttons) can be clicked. Destructive actions keep their safety gates: termination requires confirmation, and force-kill requires typing `KILL`. Single-click only; double-click and drag do nothing.

## Platform and privileges

Ports releases target macOS on Apple silicon and Intel. Linux and Windows are not release targets. Ports runs as an ordinary user and does not require `sudo` or a root install. macOS may deny details or termination of processes owned by another user or protected by the system; Ports reports that failure rather than requesting privilege escalation.

## Releases and Homebrew tap updates

Pushing a `vX.Y.Z` tag runs [`.github/workflows/release.yml`](.github/workflows/release.yml). The workflow builds the `aarch64-apple-darwin` and `x86_64-apple-darwin` targets, packages one executable per architecture, publishes both archives plus `checksums.txt` to the GitHub release, and renders the exact tag URLs and SHA-256 values into `Formula/ports.rb` in `noahlin34/homebrew-tap`.

The repository needs one user-provided Actions secret:

- `HOMEBREW_TAP_TOKEN`: a GitHub fine-grained personal access token with **Contents: Read and write** permission for `noahlin34/homebrew-tap`.

`GITHUB_TOKEN` is the repository-provided Actions token; it is used automatically for the GitHub release and must retain **Contents: Read and write** workflow permission. Add `HOMEBREW_TAP_TOKEN` under **Settings → Secrets and variables → Actions**, then push a tag such as:

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

The tag must contain three numeric dot-separated components (`v1.2.3`). No manual checksum edits are needed: the tap formula is generated from the committed `Formula/ports.rb` architecture template after the release archives are built.
