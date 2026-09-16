# M104 — The picture a model is sent

## Goal

User, 2026-09-16, after M103 and the size measurements on the
`blender_demo` PNG (2213 KB as PNG, 536 KB as JPEG q85, the model unable
to tell them apart): "可以 按照opencode来搞吧". After this milestone every
picture that reaches the journal on its way to a model is inside a
2000×2000 box and under 1 MB of encoded bytes, by opencode's ladder; a
picture already inside both is byte-identical to what came in. One brick
in `bingo-pictures`, called from its three doors and from the two
producers that bypassed them. ADR-0062.

## Bricks, in build order

1. `bingo-pictures/src/bounded.rs` (new) — `pub const MODEL_BOX: (u32,
   u32) = (2000, 2000)`, `pub const MODEL_BUDGET: usize = 1_000_000`,
   `pub fn bounded(media_type: &str, bytes: &[u8]) -> Result<Image,
   PictureError>`. Pure: (a) `media_type` known, bytes within the box
   (PNG by `png_size`, others by one decode) and `bytes.len() <=
   MODEL_BUDGET` → `Image::from_bytes(media_type, bytes)` untouched;
   (b) else decode once, `resize` into the box with `Lanczos3` when
   bigger; (c) `ladder(picture)`: PNG (default compression), then JPEG
   at `[85, 75, 65, 55, 45]` over the picture flattened on white, first
   under budget wins; (d) none → `resize` to three quarters and (c)
   again, at most `SHRINKS = 8` times; (e) still none →
   `PictureError::TooBig { bytes }`. Each step its own function under
   60 lines: `within`, `fitted_to_box`, `as_png`, `as_jpeg`,
   `flattened`, `ladder`, `bounded`. Result media type is the input's
   when untouched, else `image/png` or `image/jpeg`; `path` is `None`
   (the caller sets it as today).
   Tests (drawn pictures from `testing::drawn` plus a seeded-noise
   helper for an incompressible one): a small PNG comes back
   byte-identical with its media type; a 4000×3000 lands at 2000×1500
   (aspect kept); a noisy 1900×1900 PNG over budget comes back JPEG
   under budget; a picture with alpha comes back JPEG on white with
   no alpha channel; a BMP inside the box comes back PNG (still
   decoded, table unknown); a bounded picture fed back is untouched;
   the ladder gives up with `TooBig` when even the eighth shrink is over
   budget (use a tiny `budget` argument through a `pub(crate) fn
   bounded_within(media_type, bytes, box, budget)` the public one calls
   with the constants).
2. `bingo-pictures/src/accepted.rs` — `sniffed` and `accepted` answer
   with `bounded(...)`; `load.rs` the same for a path and a URL. The
   existing tests that assert "the bytes are the ones handed over" hold
   for pictures inside the box and budget; add one per door with a
   picture that is not.
3. `bingo-tool-fs/src/read.rs` — depends on `bingo-pictures`; the image
   arm calls `bingo_pictures::bounded(media_type, &bytes)` on a blocking
   thread (`tokio::task::spawn_blocking`), then `.at(&path)`. The 8 MB
   file cap stays; the "image too large" test becomes "a 6 MB photo is
   read and bounded". Tests: a big PNG file comes back under
   `MODEL_BUDGET` with `image/jpeg` or `image/png`; a small one
   byte-identical.
4. `bingo-mcp/src/tool.rs` — an MCP `ContentBlock::Image` goes through
   `bounded` (blocking thread), refused in words when it is not a
   picture. Test with a drawn picture over the box.
5. Black-box `crates/bingo/tests/cli/images.rs`: an `@big.png` of
   3000×3000 handed to `--print` reaches the journal under
   `MODEL_BUDGET`; the existing small-picture cases stay byte-identical.
6. Records: ADR-0062 (written first); ADR-0041 §2 and ADR-0052 §2 gain
   one dated note each; `docs/adr/README.md` index line; the
   `bingo-pictures` crate doc gains the sentence about the bound.

## Files

- `crates/bingo-pictures/src/{bounded.rs (new),lib.rs,accepted.rs,load.rs}`
- `crates/bingo-tool-fs/{Cargo.toml,src/read.rs}`
- `crates/bingo-mcp/{Cargo.toml,src/tool.rs}`
- `crates/bingo/tests/cli/images.rs`
- `docs/adr/{0062-the-picture-a-model-is-sent.md,0041-the-picture-from-anywhere.md,0052-the-picture-is-a-file.md,README.md}`

## Exit criteria

- [x] `bounded` unit tests pass, including the byte-identical, the
      aspect, the JPEG fallback, the alpha, the idempotence and the
      `TooBig` cases.
- [x] The `blender_demo` PNG (`~/tmp/blender_demo/assets/
      voxel_village_reference.png`, 2 212 534 bytes, 1312×1199) through
      `bounded` is `image/jpeg` under 1 MB; pasted into the plan's
      Verified section with the size.
- [x] `Read` on that file returns the bounded picture; a 200 KB PNG
      returns the file's own bytes.
- [ ] `scripts/budget.sh` unchanged (no new crate); `check_discipline`
      ok; every gate green; `cargo check -p bingo-pictures -p
      bingo-tool-fs -p bingo-mcp --all-targets --target
      x86_64-pc-windows-msvc` compiles.

## Non-goals

- A setting for box or budget; a per-model box.
- Bounding a picture a wire client sends inline (ADR-0040 §3 stands).
- Rewriting pictures already in a journal.
- Telling the model or the person that a picture was shrunk.
- Any change to what a surface draws; the TUI's thumbnail path
  (`fitted`) keeps Triangle and its own box.

## Risks

- R-text: a screenshot that cannot fit as PNG under 1 MB goes JPEG and
  small text softens. Accepted in ADR-0062; PNG is tried first at every
  size, so flat-colour screenshots stay PNG.
- R-time: decode + Lanczos3 on a 12 MP photo is hundreds of
  milliseconds; every caller runs it off the async runtime's threads.
- R-hash: a pasted picture's file is written before bounding (ADR-0052
  §1), so the file's hash names the original; the journal's data is the
  bounded copy. The TUI viewer opens the file.
- R-gif: an animated GIF loses its frames after the first; the table's
  `image/gif` is kept only when the picture is untouched.

## Verified (2026-09-16)

- `cargo fmt`, `cargo check` and `cargo clippy -- -D warnings`, each
  over the workspace and all targets, `--locked`: all clean.
- `cargo test -p bingo-pictures -p bingo-tool-fs -p bingo-mcp --locked`:
  257 passed, 0 failed. `cargo test --workspace --locked
  --no-fail-fast`: 4856 passed, 0 failed, 2 ignored. No flake.
- `bounded` unit tests: 9, all of the cases above, 1.9 s — the ladder
  walked whole through `bounded_within` against a small box and budget,
  because a 12 MP picture in a debug build is seconds and a test that
  spends seconds pins the machine it was written on.
- The `blender_demo` PNG, 2 212 534 bytes of 1312×1199, through
  `bounded`: **`image/jpeg`, 393 775 bytes** — inside the box already,
  so no resize, and the first JPEG quality fit; 48 ms in release,
  1.07 s in debug. `Read` on it answers with the same picture (a
  throwaway `#[ignore]` test, run once and removed); a 200 KB PNG
  comes back as the file's own bytes.
- `scripts/check_discipline.sh`: `discipline ok`, no new function over
  60 lines. `scripts/budget.sh`: `budget ok`, 342 unique dependencies
  (max 342) — unchanged, `Cargo.lock` gaining lines and no package.
- **Not verified: the Windows cross-check.** `cargo check -p
  bingo-pictures -p bingo-tool-fs -p bingo-mcp --all-targets --target
  x86_64-pc-windows-msvc` fails in `aws-lc-sys`'s build script (`fatal
  error: 'windows.h' file not found`) — the ADR-0041 2026-09-04 note's
  problem, `reqwest` under `bingo-pictures`. Already so for
  `bingo-pictures` and `bingo-mcp`; `bingo-tool-fs` joins them. CI's
  `windows` job is the backstop, and nothing here is platform-shaped.
- Two behaviours changed beyond the plan, both because a door that
  bounds must decode: a stream-json `image` block whose base64 is not a
  picture, and `Read` on a `.png` no decoder reads, are now refused by
  name. Three tests that handed over `"/9j/"` or `"iVBOR"` now hand
  over drawn pictures.
- *Follow-up, same day:* every caller of a bounding door is now off the
  runtime's threads. `bingo_pictures::seen(bytes)` and `taken(image)` —
  `sniffed` and `accepted` on a blocking thread — are public beside
  `load`, and `bingo-tool-web`'s `body::render`, the Feishu attachment
  fetch, `--print`'s `parse_line` and the MCP bridge all go through
  them; the first and third became async to do it. `bingo-tool-fs`
  keeps its own `spawn_blocking`: it knows the media type from the path
  and calls `bounded`, not a sniffing door.
