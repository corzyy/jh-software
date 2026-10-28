# JH Software

A fast software store for Fedora, written in Rust with GTK 4 and libadwaita.
It installs, updates and removes apps from **RPM repositories**, the
**Terra** repository and **Flatpak** remotes, and prefers native packages
(RPM, Terra) over Flatpak by default.

## Features

- **Sidebar navigation**: Discover, Library and Updates (with a badge), plus
  every category with subcategory filters; it folds into an overlay on narrow
  windows.
- **Discover**: a daily Spotlight grid, native-on-Fedora picks, recently
  updated apps, hand-picked apps and per-category sections.
- **Search** across ~7000 apps and Terra packages in a few milliseconds
  from the always-visible search field (results open with <kbd>Enter</kbd>;
  typing anywhere starts a search).
- **App pages** in two columns: screenshots, translated descriptions, release
  notes, add-ons and ODRS ratings and reviews on the left; an "At a Glance"
  list (source, size, safety, age rating), details (license, version,
  installed/download size), links and Flatpak permissions on the right; more
  apps by the same developer below.
- **Source selector** per app (e.g. `Fedora · RPM`, `Terra`, `Flathub · Flatpak`),
  defaulting to your configured priority: RPM → Terra → Flatpak.
- **Library**: all installed apps with size, version, source and uninstall.
- **Updates**: Flatpak apps and runtimes, RPM app updates, and OS updates.
  OS updates use PackageKit offline updates ("Download & Restart") like GNOME
  Software, or can be applied live.
- **Software Repositories**: enable or disable dnf repos and Flatpak remotes,
  and add `.flatpakrepo` files.
- **Local files**: open `.rpm`, `.flatpakref` and `.flatpakrepo` files, plus
  `appstream://` links.
- **Background mode**: autostart, periodic update checks, desktop
  notifications, and optional automatic Flatpak updates.
- Adaptive layout down to phone width, keyboard shortcuts, dark mode.

## How it works

| Part | Implementation |
|------|----------------|
| RPM / Terra transactions | PackageKit over D-Bus (polkit authentication, dnf5 backend) |
| Flatpak | libflatpak (system and user installations) |
| App metadata | AppStream from `/usr/share/swcatalog`, `/var/cache/swcatalog` and Flatpak remotes, parsed with a streaming parser and cached in `~/.cache/jh-software/catalog.bin` |
| Terra | Terra ships no AppStream data, so its packages come from the dnf5 cache and are matched to AppStream apps by name. Unmatched ones are searchable as plain packages. |
| Installed RPMs | `rpm -qa` (vendor tag identifies Terra packages) |
| Reviews | Open Desktop Ratings Service (read-only) |

A cold start reads the cached catalog in about 0.1 s. A full rebuild takes
about 0.6 s and runs in the background only when the metadata changes.

## Build and install

```sh
sudo dnf install rust cargo gtk4-devel libadwaita-devel flatpak-devel
make install-user          # installs into ~/.local, no root needed
# or system-wide:
sudo make install          # PREFIX=/usr/local by default
```

Then start **JH Software** from the app grid. `make uninstall-user` or
`sudo make uninstall` removes it again. An RPM spec is in `jh-software.spec`.

## Command line

```
jh-software                      open the store
jh-software --updates            open the updates page
jh-software --details ID         open an app (AppStream id, flatpak id or package name)
jh-software --search TERM        search
jh-software FILE.rpm|.flatpakref|.flatpakrepo
jh-software appstream://ID
jh-software --background         run hidden and notify about updates
jh-software --self-test          print backend diagnostics
```

Preferences are stored in `~/.config/jh-software/config.json`.
