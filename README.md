# ufpm — Unofficial Foundry Package Manager

An unofficial CLI package manager for [FoundryVTT](https://foundryvtt.com), written in Rust.

Foundry's built-in package manager downloads the entire package list on every visit, gives no
update preview, reports nothing when a bulk update fails halfway, and cannot tell you whether a
package is still used by any world. `ufpm` fixes all of that from the command line:

- **Cached package index** — the slow, unpaginated API call happens once a day (or on
  `ufpm cache update`), then everything is instant.
- **Update preview** — `ufpm module outdated` shows what *would* change before anything changes.
- **Transactional installs** — downloads are resumable, archives are validated, and a failed
  install or update always restores the exact previous state.
- **Usage detection** — `ufpm module unused` lists what no world uses; `--prune` deletes it
  (and refuses to guess when a world can't be inspected).
- **Dependency resolution** — hard requirements install automatically, recommendations are one
  consolidated prompt, never twenty.

> **Early development.** Make backups before pointing any tool, including this one, at a Foundry
> data directory you care about. Close Foundry before installing/removing packages.

## Installation

```sh
cargo install --git https://github.com/dealloc/ufpm
```

Requires a licensed FoundryVTT installation on the same machine: `ufpm` authenticates against the
package API with your `Config/license.json` (treated as opaque, never logged).

## Quickstart

```sh
ufpm doctor                       # where is Foundry, is the license found, cache state
ufpm cache update                 # fetch the module + system indexes (one slow call each)
ufpm module search dice           # search by name, title or author
ufpm module info dice-so-nice     # everything the index knows about one package
ufpm module add dice-so-nice       # resumable download, transactional install, deps included
ufpm module outdated              # what has updates (exit 1 with --check, for scripts)
ufpm module update                # apply all provably-newer updates
ufpm module unused --prune        # delete what no world uses, after one confirmation
```

Every `module` command exists for `system` too: `ufpm system outdated`, `ufpm system add pf2e`, …

## Commands

| Command | Description |
|---|---|
| `cache update` / `info` / `clear` | Manage the cached package index |
| `module list [--installed] [--owned] [--limit N]` | List packages (badges: protected/owned/update) |
| `module search <query> [--installed] [--owned]` | Case-insensitive search |
| `module info <name>` | Details for one package |
| `module add <name>…` | Add packages + required dependencies |
| `module outdated [--check]` | Preview available updates |
| `module update [<name>…]` | Update named packages, or everything outdated |
| `module remove <name>…` | Remove packages (confirms first) |
| `module unused [--prune]` | List (and optionally delete) packages no world uses |
| `doctor` | Diagnose paths, license and cache |

### Global flags

| Flag | Effect |
|---|---|
| `--data-path <PATH>` | FoundryVTT root (env: `UFPM_DATA_PATH`); otherwise auto-discovered |
| `-y`, `--yes` | Skip confirmations (`--yes` never auto-accepts *recommended* packages) |
| `-v` / `-vv` / `-vvv` | Increasing verbosity on stderr |
| `-q`, `--quiet` | Errors only |
| `--no-progress` | Plain status lines instead of progress bars |

Command *data* goes to stdout, everything else (progress, warnings, summaries) to stderr — so
`ufpm module list | grep pf2e` sees only rows.

## Behaviour worth knowing

- **FoundryVTT owns the installation.** The on-disk state is the only source of truth; `ufpm`
  keeps no lockfile and rescans `Data/` on every run. Anything Foundry installs, updates or
  removes is picked up automatically.
- **Old versions don't exist.** The Foundry API only serves the latest release of each package,
  so there is no `install <name>@<version>` and no downgrade. A failed update restores the
  previous version from its transactional backup; a *successful* one cannot be rolled back.
- **`update` is conservative in bulk.** With no names, only provably-newer versions are applied;
  packages whose local version merely *differs* are skipped and must be updated by name.
- **Prune never guesses.** Reading a world's enabled-module list requires its LevelDB database
  (always inspected via a temporary copy — the original is never opened). If any world cannot be
  read, affected packages are "possibly unused" and `--prune` refuses to delete them.
- **The Foundry version is currently a constant** (`14.362`), used for API requests and
  compatibility warnings. Override with `UFPM_FOUNDRY_VERSION` if you run something else.
- S3-backed installations (`awsConfig`) are detected and rejected for now.

## Shell completions

```sh
ufpm completions zsh > ~/.zfunc/_ufpm   # also: bash, fish, powershell, elvish
```

## Contributing

```sh
just setup      # install the toolchain helpers (nextest, deny, machete, …)
just precommit  # format, lint (pedantic, warnings deny), deps, commits, tests
```

The codebase is documented down to private items and must pass `just precommit` cleanly;
`PLAN.md` describes the architecture. Commit messages follow conventional commits.

## License

See [LICENSE](LICENSE.md) (AGPL-3.0).
