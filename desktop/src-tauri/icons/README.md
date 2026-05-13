# Icons

Source artwork is the daccord mascot, taken from
`daccord/assets/icons/icon_1024x1024.png`. The canonical source committed
here is **`icon.png`** (1024×1024); the smaller PNGs and `icon.ico` are
direct copies from the same source.

`icon.icns` (macOS) is generated at build time in CI by:

```
pnpm tauri icon icons/icon.png
```

That command regenerates the full Tauri icon set from `icon.png`, including
the `.icns` archive. To refresh icons locally, run the same command from
`desktop/`.
