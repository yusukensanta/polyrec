# Bundled fonts

PolyRec's UI typeface. Both files are compiled into `polyrec.exe` (`include_bytes!` in
`src/ui/dashboard/theme.rs`), so the UI looks the same on every machine. They don't depend
on which fonts the user has installed.

| File | Family | Upstream release | SHA-256 |
|---|---|---|---|
| `IBMPlexSans-Regular.ttf` | IBM Plex Sans (Latin) | [`@ibm/plex-sans@1.1.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans%401.1.0), `packages/plex-sans/fonts/complete/ttf/` | `975dcda37d80f038dcd143c22e33ca2d97a0cc5a929aace1c749153b0fe1afa5` |
| `IBMPlexSansJP-Regular.ttf` | IBM Plex Sans JP (Japanese) | [`@ibm/plex-sans-jp@3.0.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans-jp%403.0.0), `packages/plex-sans-jp/fonts/complete/ttf/unhinted/` | `0168493ae143d16e4c0d374c090b1ee5eab9ee475536d2e429368dbcfc4a6abe` |

## License

Both fonts are © IBM Corp. and licensed under the SIL Open Font License 1.1, with Reserved
Font Name "Plex". The full text is in [`OFL.txt`](OFL.txt). It is byte-identical to each
package's `LICENSE.txt`, and the release workflow ships it as `LICENSE-IBM-Plex.txt` in the
zip and the installer, as the OFL requires.

## Why these exact files

- **Unmodified, not subset.** Subsetting Plex Sans JP would cut about 5.7 MB, but under the
  OFL a subset is a "Modified Version", and the Reserved Font Name forbids shipping it under
  the "Plex" name. The unit test `bundled_plex_fonts_are_the_unmodified_upstream_files` pins
  the hashes above, so an accidental modification fails CI.
- **Unhinted JP build.** egui rasterizes outlines itself and ignores TrueType hinting, so the
  hinted build would only add bytes.
- **Regular weight only.** egui has no bold synthesis. The theme expresses emphasis through
  color and size, so one weight per script covers the whole UI.

## Updating

1. Download the new `.ttf` files from the release commit of the `@ibm/plex-sans@X` or
   `@ibm/plex-sans-jp@X` tag in [IBM/plex](https://github.com/IBM/plex).
2. Replace the files here, then update the table above and the hashes in that test.
3. Run `cargo test` to check coverage. `bundled_fonts_cover_every_ui_string` fails if any
   character in `src/i18n.rs` lacks a glyph in the bundled fonts.
