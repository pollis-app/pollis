# X11 clipboard repro (#1794)

A standalone crate (its own `[workspace]`, not part of the app build). It times the old and new clipboard paste reads against a fake X11 clipboard owner that copies Steam's "Copy to clipboard" behaviour (ValveSoftware/steam-for-linux#11476):
- Steam offers only `TARGETS` and `text/uri-list`.
- It answers the `text/uri-list` request with the data typed `UTF8_STRING`.

- `src/bin/fake_steam.rs`: the fake owner. Its mode sets how it treats formats it doesn't offer:
  - `ignore`: never answers.
  - `refuse`: answers with "no data", the ICCCM-correct reply.
  - `anyutf8`: answers every request with the uri-list.
  - `slow`: like `refuse`, but each reply takes 1.5 s.
- `src/bin/probe.rs`: times the old reads (arboard `get_text` / `get_image`) and the new bounded `clipboard::x11::read_offer`.
- `src/lib.rs`: compiles `src-tauri/src/clipboard.rs` directly; `tauri-stub` stands in for the one Tauri API it uses.

Run it with `./run.sh`, which needs `xvfb` and `xclip`. Expected results:
- The OLD reads take several seconds in `ignore` mode, about 24 s for `get_text` (6 formats × 4 s).
- The NEW reader returns the screenshot path in well under 3 s in every mode.
