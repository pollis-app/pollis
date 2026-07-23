# `/learn` — explainers + animations (Epic #589)

Plain-language explainers for everything Pollis asks users to trust, each with an
animation you can watch and (where poking beats watching) a widget you can drive.
The pages live in `website/learn.html`; this directory holds the **animation
sources** and the **narration scripts**.

Topic 7 (Merkle trees, #596) is the **M0 pilot** — it establishes the pattern
below. Every later topic follows it.

## What ships per topic

Each topic adds one `<section>` to `website/learn.html` plus, under
`website/learn/media/<slug>.*`:

| Artifact | Source | Notes |
| --- | --- | --- |
| `<slug>.mp4` / `<slug>.webm` | `learn/manim/scenes/<slug>.py` → `render.sh` | **no audio track** (added later); flat vector, 720p30 |
| `<slug>.jpg` | `render.sh` (frame ~35% in) | `<video poster>` |
| `<slug>.vtt` | hand-authored from `learn/manim/scripts/<slug>.md` | carries the narration until audio exists |
| on-page transcript | same script | in a `<details>` — page teaches without JS or video |

The narration script (`scripts/<slug>.md`) is the **single source of truth** for
the `.vtt`, the on-page transcript, and future voiceover. Its beat timings match
the `hold_until(...)` calls in the scene, so the video is paced to be *read*, not
just watched.

## Toolchain

Manim Community Edition in an isolated `uv` venv (system Python is untouched):

```bash
cd learn/manim
uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python manim   # pulls pycairo/manimpango/scipy…
```

`ffmpeg` (system) does the WebM transcode + poster extraction. `.venv/` and
Manim's scratch `media/` are git-ignored; only `website/learn/media/*` is committed.

## Render

```bash
learn/manim/render.sh <SceneClass> <slug> [l|m|h|k]     # default m = 720p30
# e.g.
learn/manim/render.sh MerkleTrees merkle-trees m
```

This renders with no audio, transcodes VP9 WebM, grabs a poster, and drops all
three into `website/learn/media/`. Then hand-author `<slug>.vtt` from the script.

## Where the rendered artifacts live

For now the `.mp4`/`.webm`/`.jpg` are **committed to git** and served straight
from the git-connected Cloudflare Pages build (`website/` is the Pages root, no
build step). A full 720p topic is ~5 MB.

The epic's long-term plan is to move rendered artifacts to **R2** to keep clone
size sane (~5 MB × 12 topics). That's a one-line swap: introduce a media base-URL
constant in `learn.html` and point the `<source>`/`poster`/`track` `src`s at
`https://<r2-host>/learn/…` instead of the relative `learn/media/…`. No R2 upload
pipeline is wired yet, so until it is, git is the store and the deployed page
works with zero extra infra.

## Conventions (so every topic feels like one section)

- Palette in the scenes mirrors `website/styles.css` (amber `#fdba74`, bg
  `#0f1117`, danger/ok greens/reds) — video and page look like one thing.
- Animations use **real** live data where the topic references it (Topic 7 shows
  the actual `verify.pollis.com` binaries root). Update the `LIVE_ROOT` constant
  if the log is re-seeded.
- Widgets are vanilla JS + SVG + `crypto.subtle`, no dependencies, ship `hidden`
  and are revealed by `learn.js` only when they can actually run (progressive
  enhancement — the prose + video teach without them).
- Every section states its mechanism's **honest limit** in the `.ln-limit`
  callout. Non-negotiable (epic "honesty constraint").
- Every claim traces to a repo anchor, listed at the top of the scene file and in
  the topic's issue.
