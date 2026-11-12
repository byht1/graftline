Graftline 0.1.0 is the first release: a desktop Git client built around the commit graph.

## Downloads

| System | File | Notes |
|---|---|---|
| **Windows 10/11** | **`Graftline-Setup-x64.msi`** | Installer. It shows where the program and your settings are stored. |
| Windows | `graftline.exe` | Portable: a single file, no installation. |
| **macOS 11+** | **`Graftline-macOS-universal.dmg`** | Apple Silicon and Intel. |
| **Linux (Debian/Ubuntu)** | **`graftline-linux-amd64.deb`** | `sudo apt install ./graftline-linux-amd64.deb` |
| Linux (other) | `graftline-linux-x86_64.tar.gz` | Needs WebKitGTK 4.1 and GTK 3; see README.txt inside. |
| | `SHA256SUMS.txt` | Checksums. |

## What it does

- **Commit graph** with branches, tags, stashes and remotes. Search by message, author or hash. Several repositories in tabs.
- **Working tree:** stage, unstage and discard by file, hunk or line. Commit and amend. Unified and side-by-side diff with syntax highlighting.
- **Branches and remotes:** create, check out, rename, delete; fetch, pull, push with progress; clone; tags and stashes. Drag a branch onto a branch to merge or rebase.
- **History:** merge, rebase, interactive rebase, cherry-pick, revert, reset.
- **Conflicts:** take a side for a file, a block or a line, or edit the result by hand.
- **File history, blame, command palette, keyboard shortcuts, eight themes.**

## Your work is safe

- Discard, `reset --hard` and abort save the affected files in a snapshot first, and the notification has **Undo**.
- Force push is always `--force-with-lease` and asks first.
- Deleting a branch lists the commits that exist nowhere else.
- A failed Git command is shown word for word, with its command line.

## Where settings are stored

| System | Folder |
|---|---|
| Windows | `%APPDATA%\graftline\` |
| macOS | `~/Library/Application Support/graftline/` |
| Linux | `~/.config/graftline/` |

The folder holds `recent.json`, `ui-prefs.json`, `window.json`, `cache/`, `graftline.log` and the `webview/` cache. Graftline stores no passwords.

## Notes

- **Git.** Fetch, pull, push, clone and history changes use the Git on your computer. Without Git, Graftline still browses repositories.
- **The builds are not code-signed yet.**
  - Windows SmartScreen: choose *More info → Run anyway*.
  - macOS: right-click the app → *Open*, or run `xattr -dr com.apple.quarantine /Applications/Graftline.app`.
- **WebView2.** The Windows app needs the WebView2 runtime, which is preinstalled on Windows 10/11.
- **Known limits** are listed in the README.
