# Sivro

[![CI](https://github.com/MohamedElashri/sivro/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/MohamedElashri/sivro/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/MohamedElashri/sivro?include_prereleases&sort=semver)](https://github.com/MohamedElashri/sivro/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A local, read-only desktop explorer for tabular data, built with Rust, Tauri 2
and DuckDB.

- **Opens** Parquet, HDF5, CSV, TSV, JSON, JSON Lines, Excel (`.xlsx`, `.xlsm`,
  `.xlsb`, `.xls`) and SQLite.
- **Exports** the current view to Parquet, HDF5, CSV, TSV, a JSON array or JSON Lines.
- **Safe by design:** it never modifies a source file, runs no macros or
  formulas and accepts no SQL. Exploring data works entirely offline.
- **Exact values:** large integers and decimals are never rounded through
  floating point.
- **Appearance:** Catppuccin Latte and Mocha themes, a saved Light/Dark/System
  choice, visible menu hover and keyboard highlights, and clear pending-view
  indicators on tabs.
- **Manual updates:** About → Check for updates contacts GitHub only when
  requested. Review the release notes and confirm before downloading and
  installing a signed update. There are no automatic checks or updates.

Sivro is a pre-1.0 beta; see the [changelog](CHANGELOG.md) for
known limitations.

## Install

**Linux and macOS:**

```sh
curl -fsSL https://raw.githubusercontent.com/MohamedElashri/sivro/main/scripts/install.sh | sh
```

The script picks the right package (`.deb`, `.rpm` or AppImage on Linux, the
universal `.dmg` on macOS), verifies it against `SHA256SUMS.txt` and installs
it. Set `SIVRO_VERSION=v0.1.1` to pin a release.

**Manual download:** get the package for your platform from the
[releases page](https://github.com/MohamedElashri/sivro/releases).

| Platform | Package |
|---|---|
| Windows x86_64 | `.msi` or `-setup.exe` |
| macOS (Intel and Apple Silicon) | universal `.dmg` |
| Linux x86_64 | `.deb`, `.rpm` or `.AppImage` |

> [!NOTE]
> **macOS:** the app is not signed with an Apple Developer ID or notarized, so
> macOS blocks a downloaded copy ("Sivro is damaged" or "cannot be opened").
> After moving it to Applications, allow it with:
>
> ```sh
> xattr -dr com.apple.quarantine /Applications/Sivro.app
> ```
>
> or open it once and choose **Open Anyway** under System Settings → Privacy &
> Security. The install script does this for you.
>
> **Windows:** the installer is not code-signed either, so SmartScreen may
> warn; choose **More info → Run anyway**.

## Usage

1. Open a file with **Open** (`Ctrl/⌘+O`) or drag it onto the window. Each file
   opens in its own tab.
2. Confirm the parsing preview for CSV/TSV, pick a sheet for Excel workbooks,
   pick a table for SQLite databases, or browse HDF5 groups and choose a dataset
   and slice.
3. Search (`Ctrl/⌘+K`), filter, sort, hide columns, profile a column or
   inspect a row. **Count rows** gives exact totals for filtered views.
4. **Export** writes the current view (columns, filters and sort) to a new file.

JSON files that aren't tables open as a navigable document tree.
[Formats and behaviour](docs/formats.md) describes each reader in detail.

## Development

Requires Node.js 24, Rust 1.90+ and the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your
platform.

```sh
make dev      # run the app with hot reload
make ci       # formatting, clippy and all tests, as CI runs them
make build    # release packages
make clean    # remove build outputs
make help     # list every target
```

CI lints with the latest stable Rust, so run `rustup update stable` first. On
Windows the Makefile builds DuckDB from source automatically. See
[development](docs/development.md) for architecture, tests and releasing.

## License

[MIT](LICENSE).
