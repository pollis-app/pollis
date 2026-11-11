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
| `<slug>.vtt` | generated from `learn/manim/scripts/<slug>.md` by `node scripts/check-learn-sync.mjs --write` | carries the narration until audio exists; in git, served with the page |
| on-page transcript | same script, same command | in a `<details>`, one `<p>` per beat — page teaches without JS or video |

The narration script (`scripts/<slug>.md`) is the **single source of truth** for
the `.vtt`, the on-page transcript, and future voiceover. Its beat timings match
the `hold_until(...)` calls in the scene, so the video is paced to be *read*, not
just watched. Edit the script, then run `node scripts/check-learn-sync.mjs --write`.
CI (`scripts-check.yml`) fails if a caption cue or a transcript paragraph differs
from its beat (#1723).

## Toolchain

Manim Community Edition in an isolated `uv` venv (system Python is untouched):

```bash
cd learn/manim
uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python manim   # pulls pycairo/manimpango/scipy…
```

`ffmpeg` (system) does the WebM transcode + poster extraction. `.venv/` and
Manim's scratch `media/` are git-ignored, and so are the rendered
`website/learn/media/*.{mp4,webm,jpg}`, which go to R2 (below). Only the `.vtt`
files there are committed.

## Render

```bash
learn/manim/render.sh <SceneClass> <slug> [l|m|h|k]     # default m = 720p30
# e.g.
learn/manim/render.sh MerkleTrees merkle-trees m
```

This renders with no audio, transcodes VP9 WebM, grabs a poster, and drops all
three into `website/learn/media/`. The `.vtt` comes from the script, not the render.

## Where the rendered artifacts live

The `.mp4`/`.webm`/`.jpg` live on **R2** and are served from
`https://cdn.pollis.com/learn/media/…`. #697 moved them out of git, where they
took up 77 MB of the clone. The upload steps and their traps are in
`docs/deployments.md` → "`/learn` section". Objects are cached immutably, so a
re-render needs a new file name. The `.vtt` caption tracks stay in git and
deploy with the page.

## Conventions (so every topic feels like one section)

- Palette in the scenes mirrors `website/styles.css` (amber `#fdba74`, bg
  `#0f1117`, danger/ok greens/reds) — video and page look like one thing.
- Animations use **real** live data where the topic references it (Topic 7 shows
  the actual `verify.pollis.com` binaries root). Update the `LIVE_ROOT` constant
  if the log is re-seeded.
- Widgets are vanilla JS + SVG + `crypto.subtle`, no dependencies, ship `hidden`
  and are revealed by `learn.js` only when they can actually run (progressive
  enhancement — the prose + video teach without them). One component is mounted
  per `[data-merkle-widget]` element, so topics share it rather than forking it:
  Topic 7 mounts it plain, Topic 8 adds `data-proof` for the inclusion-proof walk
  (pick an entry, get only its sibling hashes, climb, tamper, watch it fail).
- Every section states its mechanism's **honest limit** in the `.ln-limit`
  callout. Non-negotiable (epic "honesty constraint").
- Every claim traces to a repo anchor, listed at the top of the scene file and in
  the topic's issue.
