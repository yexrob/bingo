# Message avatars

Eight portraits, bundled into the binary and shown next to a sender's name in the
workspace conversation (`src/tui/avatar.rs`).

- **Style**: [Lorelei](https://www.dicebear.com/styles/lorelei/) by Lisa Wischofsky,
  rendered through the [DiceBear](https://www.dicebear.com/) HTTP API.
- **Licence**: [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) — public
  domain, no attribution required. Credited here because knowing where an asset came
  from is worth more than the licence obliges.

Each file was fetched once and committed; nothing downloads at runtime.

```
https://api.dicebear.com/9.x/lorelei/png
  ?seed=<seed>&size=96&radius=50&scale=125
  &backgroundColor=<hex>&backgroundType=solid
```

| file | seed | background |
|---|---|---|
| `emi.png` | Emi | `b6e3f4` |
| `kenji.png` | Kenji | `ffdfbf` |
| `sora.png` | Sora | `ffd5dc` |
| `mika.png` | Mika | `c0aede` |
| `taro.png` | Taro | `a7f3d0` |
| `jin.png` | Jin | `fde68a` |
| `kai.png` | Kai | `fecaca` |
| `rio.png` | Rio | `d1d4f9` |

The file name is the portrait's id: `.bingo/team.json` pins one to a member with
`"avatar": "sora"`, so a crew member keeps the same face across sessions instead of
being handed whatever a hash of its name lands on.

Why these parameters and these eight: the set is chosen for **silhouette contrast at
36×38 pixels**, which is the real size on screen — glasses, a beard, a pale crop, a
bob, long straight hair. At that size the outline and the background tint identify a
sender; the face does not. `radius=50` makes the circular chip and `scale=125` crops to
the head, since the default framing spends most of a small avatar on shoulders.
