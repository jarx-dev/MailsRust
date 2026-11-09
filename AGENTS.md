# Poczta

A native Windows mail, calendar and contacts client in Rust (eframe/egui 0.37, redb, WebView2). One EXE; Polish UI.

## What we never compromise on

**Care.** People read their real mail in this app every day. A lost message, a wrong "read" mark, a reply sent to the wrong person or a window that will not come back is worse than any missing feature. Prefer the smallest change that fully solves the request, finish every place it must reach, and prove it before calling it done.

**Performance.** Startup shows the mailbox in about 60 ms and idles near 0% CPU (see `PERF.md`). No frame may wait on the network, the disk or a lock; slow work goes to a background thread and comes back through a channel. No continuous repaint: wake the UI with `request_repaint_after` or a channel. A change that adds memory, startup time or per-frame work must say how much, measured on the same scenario before and after.

## Commands

- Build on Linux: `cargo xwin build --release --target x86_64-pc-windows-msvc`. Nothing runs on Linux, including `cargo test`; verify on Windows.
- Test on the Windows laptop: `scripts/win-test.sh build`, then `scripts/win-test.sh run <ms> [VAR=value…]` (offline copy of the mailbox, log and CPU/RAM trace come back). Machine-specific values live in `~/.config/poczta/build.env`, never in the repo.
- Screenshots: `mailclient.exe --screenshot out.png` draws demo data (`Store::sample()`); `POCZTA_START=account|calendar|about|…` and `POCZTA_DARK=1` pick the state.
- Lint: `cargo clippy --release --target x86_64-pc-windows-msvc --all-targets`. The bin has 42 known warnings; a change must not add any.

## Hit every surface

Before calling UI work done, say which of these applied: light and dark theme; a narrow reading pane; several accounts; a conversation (earlier messages folded); the separate reader window; minimized and in the tray; keyboard shortcuts and the ☰ / right-click menus that reach the same action.

## Traps

- `ui.scope_builder(max_rect…)` puts the parent cursor right under that rect, not under what you allocated before it. Content after a floating row lands too high; call `ui.advance_cursor_after_rect` afterwards.
- Hiding the window to the tray means minimizing first, then `Visible(false)`. A hidden window is still drawn in full, because winit never reports it covered.
- winit applies window flags as a diff. Showing the window with raw Win32 (`ShowWindow`) leaves winit thinking it is hidden, and its next flag change hides it again (see `shown_by_mail`).
- Repo files are mostly CRLF, but some are LF. Keep each file's line endings.
- Icons are Segoe Fluent private-use glyphs in `theme::ic`, loaded as the first fallback font. Add new ones there; emoji glyphs render differently on Windows 10 and 11.

## Never

- Never point a test at the real mailbox (`%LOCALAPPDATA%\Poczta`). Use `POCZTA_DATA_DIR` with a copy; the real log and database are read-only.
- Never commit addresses of private machines, user paths, keys or real mail content. This repository is public.
- Never publish a build without `POCZTA_DEFAULT_UPDATE_URL`, or installed copies stop updating. `scripts/publish.sh` refuses, so do not work around it. Every merge to `main` is built and shipped to users by itself, so `main` must always be releasable.
- Never kill processes by name or pattern; other copies of the app may be running. Stop only what you started.

## Pull requests

One request, one PR, opened only when asked. The title says why the change matters; the body gives the problem, the fix, and how it was proven. UI changes carry before/after screenshots, and performance changes carry before/after numbers.
