# Message avatars

Eight portraits, bundled into the binary and shown next to a sender's name in the
workspace conversation (`src/tui/avatar.rs`).

- **Style**: [Notionists](https://www.dicebear.com/styles/notionists/) by
  [Zoish](https://bio.link/heyzoish), rendered through the
  [DiceBear](https://www.dicebear.com/) HTTP API.
- **Licence**: [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) — public
  domain, no attribution required. Credited here because knowing where an asset came
  from is worth more than the licence obliges.

Each file was fetched once and committed; nothing downloads at runtime.

```
https://api.dicebear.com/9.x/notionists/png
  ?seed=<seed>&size=96&radius=50&scale=140&translateY=8
  &backgroundColor=<hex>&backgroundType=solid
```

| file | seed | background |
|---|---|---|
| `00.png` | Aneka | `b6e3f4` |
| `01.png` | Felix | `ffdfbf` |
| `02.png` | Luna | `ffd5dc` |
| `03.png` | Milo | `c0aede` |
| `04.png` | Nova | `a7f3d0` |
| `05.png` | Sasha | `fde68a` |
| `06.png` | Tiger | `fecaca` |
| `07.png` | Zoe | `d1d4f9` |

Why these parameters: `radius=50` makes the circular chip, `scale=140` + `translateY=8`
crop to the head (the default framing spends about 40% of a 36-pixel avatar on
shoulders), and one explicit `backgroundColor` per file keeps the eight tints distinct —
at avatar size the background colour separates senders faster than the face does.
