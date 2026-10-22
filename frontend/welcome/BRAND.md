# Thiscord welcome mark

`assets/thiscord-mark.png` is the transparent cobalt gathering-knot mark used in
the welcome page header and footer. `assets/favicon.png` is its browser favicon.
Generated with the built-in image generation tool; the original is retained at
`brand/thiscord-mark-original.png`. Transparent web-sized copies (128px and 32px)
were exported with `cargo tauri icon`. The source is outside the copied assets
directory, so visitors only download the small web versions.

The desktop uses the same source: `frontend/icons/icon.png` (Linux and the
default window/tray image), `icon.ico` (Windows), and `icon.icns` (macOS).
`frontend/tauri.conf.json` already references these files, and the tray uses
Tauri's default window icon. The former letter-T SVG has been removed to avoid
regenerating the old branding accidentally.

To regenerate the desktop assets from the repository root:

```powershell
cargo tauri icon frontend/welcome/brand/thiscord-mark-original.png --output target/desktop-brand-icons
foreach ($name in 'icon.png', 'icon.ico', 'icon.icns') {
    Copy-Item "target/desktop-brand-icons/$name" "frontend/icons/$name"
}
```

Only the three desktop files are retained; mobile exports are not used.

## Generation prompt

```text
Use case: logo-brand
Asset type: final standalone brand symbol for Thiscord, a desktop community voice and chat app, to use in its website header and favicon.
Primary request: Design one iconic, original abstract mark called the gathering knot. Two broad, rounded, interlocking ribbon-like hooks clasp around one compact open central space. Their union has a slightly asymmetric, confident silhouette, subtly suggesting a lowercase t without literally drawing a letter. Friendly, unmistakable, exceptionally simple geometry. Think a symbol friends could draw from memory in three strokes.
Style: flat, polished graphic design with crisp vector-like edges; solid single-color cobalt #2449D8. No gradients, shadows, bevels, texture or 3D.
Composition: single centered symbol on a genuinely transparent square canvas, fills approximately 85 percent of the canvas, generous thick strokes and wide negative spaces; must read clearly at 24 pixels.
Text: no text or wordmark.
Avoid: speech bubbles, faces, eyes, game controllers, headphones, Discord or Twitch shapes, enclosing rounded-square badges, generic chain-link/infinity icons, flowers, stars, multiple concepts or presentation boards. The silhouette itself is the identity.
```
