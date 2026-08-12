# poketo art

Generated with [SpriteCook](https://spritecook.ai), `gpt-image-2` at `quality=low`, `resolution=1K`, `pixel=true`, `smart_crop=false`. Two credits each, ten in total.

Every sheet was corrected mechanically before being committed. The raw generations are 250ish pixels square with fractional cells and are not kept: they are a step, not a source.

What each one needed, because none of it was optional and all of it was visible in the running game:

- **All of them**: resampled once with `magick -filter Lanczos` to a size whose cells divide exactly, so the renderer addresses them with integer rectangles.
- **`terrain.png`**: the prompt asked for gridlines to make the layout legible, and they had to be cropped back off. Each cell was re-cut from its interior (`54x54+5+5` of a 64-pixel cell) and rescaled. Gridlines are art the moment the game draws them, and they showed up as a lattice over the whole map.
- **`creatures.png`**: re-cut from measured bounding boxes (`magick -connected-components`) rather than by splitting the sheet in three. Bramblet's thorns spanned x=4..89 against a cell boundary at 85, so a third-split drew a sliver of Bramblet down the left edge of Quillick.
- **`trainer.png`**: every cell trimmed to its content and re-composited at one size on one baseline (`-resize 28x30 -gravity South -extent 32x32`). A generator draws each cell at its own scale and offset, and cycling those reads as a jiggle rather than as a walk. The columns are near enough the same drawing that the renderer's one-pixel bob is what actually sells the step.

Assets are **committed and embedded** with `include_bytes!` rather than fetched at runtime. A `cargo build` must never need a SpriteCook account, a missing file has to be a compile error on every target rather than a 404 in one browser, and `Host::cache_bust` stamps URLs written in `index.html`, which a texture fetched by the wasm itself never is.

| file | size | cells | asset id | source dimensions |
| --- | --- | --- | --- | --- |
| `terrain.png` | 128x128 | 4x4 of 32x32 | `159f09c5-2939-404d-9803-90202178ff89` | 250x250 |
| `trainer.png` | 96x128 | 3x4 of 32x32 | `e62d25a2-a207-469c-b8cc-725e6bd826ea` | 256x256 |
| `creatures.png` | 288x96 | 3x1 of 96x96 | `06a08904-c8a0-4479-bd44-5bbf34222aec` | 257x259, trimmed |
| `props.png` | 96x32 | 3x1 of 32x32 | `f8ef3a5b-10ca-4020-bb1f-3e7501c227e5` | 206x220, trimmed |
| `backdrop.png` | 320x180 | one image | `21cb33cf-6296-4cec-abd2-38ec0c68fdb1` | 249x147 |

The healing spring was generated separately (`16de1a68-d64a-4018-9386-dc1284e3f70d`, 81x81) and composited into `terrain.png` at cell (3,3), which had been a spare grass variant. Twelve credits in total.

`terrain.png` was generated first and passed as `style_asset_ids` to the other four, which is what keeps the palette and the outline weight consistent across a set generated one job at a time.

## Cell order

Both grids are read left to right, top to bottom, and the code that addresses them is the authority: `render::terrain_source` and `render::walk_source`.

`terrain.png`, by row: path, pale path, grass, flowering grass / tall grass, tall grass variant, shallow water, deep water / tree canopy, canopy variant, mossy stone, sand / three grass variants and the healing spring.

`trainer.png`, by row: facing south (front), north (back), east, west. Within a row: left foot forward, standing, right foot forward.

`creatures.png`, left to right: Bramblet, Quillick, Mossgab, matching `Creature::of_kind` on `kind % 3`.

`props.png`, left to right: flowers, rock, sign, matching `terrain::Prop`.
