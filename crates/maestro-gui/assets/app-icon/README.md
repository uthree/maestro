# Application icon

Maestro's icon is the Atelier element-cell mark: **Ma** on a crimson tile, register number **2**
top left, the release's `major.minor` top right and the name along the bottom. It follows
[`doc/app-icons.md`](https://github.com/uthree/design-language/blob/main/doc/app-icons.md) in the
design language and is drawn by its `tools/app-icon/make_icons.py`; never edit or recolour these
files by hand.

The register entry they were drawn from:

```toml
[[app]]
slug = "maestro"
name = "Maestro"
symbol = "Ma"
number = 2
version = "0.1.0"
element = "crimson"
ship = true
```

To redraw them (for example after a version bump), add that entry to the design language's
`tools/app-icon/apps.toml` (or pass a file holding it with `--apps`) and run

```bash
python3 tools/app-icon/make_icons.py --only maestro --no-sheets --out <dir>
```

then copy `<dir>/maestro/` over this folder. A release workflow can do the same with the
`uthree/design-language/.github/actions/app-icon` action and copy its output here before building.

Where each file is used:

| File | Use |
| --- | --- |
| `icon.ico` | Embedded in the Windows executable as resource 1 by `../windows.rc` (`build.rs`); gpui gives every window that icon. |
| `icon.icns` | The macOS bundle's `CFBundleIconFile`, beside `../../macos/Info.plist`. |
| `png/<size>.png`, `icon.svg` | Linux `hicolor/<size>x<size>/apps/io.github.uthree.maestro.png` and `hicolor/scalable`, named after the window's app id and `../../linux/io.github.uthree.maestro.desktop`. |
| `title-bar.png` | The small drawing at 64 px, first in every window's title bar on Windows and Linux (`TitleBar::app_icon`, served by `AppAssets` in `src/main.rs`). |
| `symbolic.svg` | Menu bar, tray and monochrome contexts. |
| `icon-corners.svg`, `icon-medium.svg`, `icon-small.svg`, `icon-macos.svg` | The 128 px, 48–64 px and 16–32 px drawings, and the master on Apple's grid. |

These files are redistributed from the design language under the Apache-2.0 licence in
`ATELIER-LICENSE`.
