# Contributing to GMount Drive

Thanks for your interest in improving GMount Drive! This document explains how to set up a
development environment and the conventions we follow.

## Development setup

1. **Install Rust** (stable) via [rustup](https://rustup.rs/).
2. **Install the system dependencies** (Debian/Ubuntu):
   ```bash
   sudo apt install build-essential libgtk-4-dev libadwaita-1-dev libdbus-1-dev fuse3
   ```
   On Fedora:
   ```bash
   sudo dnf install gcc gtk4-devel libadwaita-devel dbus-devel fuse3
   ```
3. **Install rclone** (≥ 1.66) on your `PATH` or at `~/.local/bin/rclone`
   (see https://rclone.org/downloads/).
4. **Build & run:**
   ```bash
   cargo build
   ./target/debug/gdrive-mount
   ```

## Project layout

The codebase is small and modular. See the **Source layout** table in the
[README](README.md#source-layout) for what each module does. In short:

- UI lives in `src/ui.rs` and `src/wizard.rs`.
- Everything that talks to rclone/Google lives in `src/rclone.rs`, `src/oauth.rs`,
  `src/mount.rs` and `src/stats.rs`.
- Persistent settings are in `src/appconfig.rs`.

## Conventions

- **Language:** code, comments, identifiers and user-facing strings are in **English**.
- **Formatting:** run `cargo fmt` before committing.
- **Linting:** keep `cargo clippy` warning-free where reasonable.
- **Small, focused commits** with clear messages (imperative mood, e.g. "Add cache size limit").
- **No secrets, ever.** Never commit tokens, OAuth client secrets, `rclone.conf`, or anything
  personal. Credentials are always provided by the user at runtime.

## Submitting a pull request

1. **Fork** the repository and create a feature branch:
   ```bash
   git checkout -b my-feature
   ```
2. Make your changes. Make sure it builds (`cargo build`) and is formatted (`cargo fmt`).
3. **Test manually** — the app drives a real `rclone` mount, so verify the behavior you
   changed actually works (mount, browse, the affected preference, etc.).
4. Push your branch and open a **pull request** against `main`, describing:
   - what the change does and why,
   - how you tested it,
   - any follow-ups or known limitations.
5. Be responsive to review feedback. We aim to keep the app **small, fast and native** — changes
   that add heavy dependencies or background overhead will get extra scrutiny.

## Reporting bugs & ideas

Open an [issue](../../issues) with:

- what you expected vs. what happened,
- steps to reproduce,
- your distribution, desktop environment, and `rclone version`.

Feature ideas are welcome too — especially anything that makes the "mount your cloud as a disk"
experience smoother.

## License

By contributing, you agree that your contributions are licensed under the project's
[GPL-3.0-or-later](LICENSE) license.
