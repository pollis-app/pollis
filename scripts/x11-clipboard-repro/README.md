# X11 clipboard repro (#1794)

A standalone crate (its own `[workspace]`, not part of the app build). It times the old and new clipboard paste reads against a fake X11 clipboard owner that copies Steam's "Copy to clipboard" behaviour (ValveSoftware/steam-for-linux#11476):
- Steam offers only `TARGETS` and `text/uri-list`.
- It answers the `text/uri-list` request with the data typed `UTF8_STRING`.

- `src/bin/fake_steam.rs`: the fake owner. Its mode sets how it treats formats it doesn't offer:
  - `ignore`: never answers.
  - `refuse`: answers with "no data", the ICCCM-correct reply.
  - `anyutf8`: answers every request with the uri-list.
  - `slow`: like `refuse`, but each reply takes 1.5 s.
  - A second argument names the file to offer; empty offers no file list. `FAKE_TEXT=<text>` (or `FAKE_TEXT_BYTES=<n>`) also offers `UTF8_STRING`, and `FAKE_INCR_CHUNK=<n>` sends text larger than n bytes over INCR.
- `src/bin/probe.rs`: times the old reads (arboard `get_text` / `get_image`) and the new bounded `clipboard::x11::read_offer` and `read_text`.
- `src/lib.rs`: compiles `src-tauri/src/clipboard.rs` directly; `tauri-stub` stands in for the one Tauri API it uses.

Run it with `./run.sh`, which needs `xvfb` and `xclip`. Expected results:
- The OLD reads take several seconds in `ignore` mode, about 24 s for `get_text` (6 formats × 4 s).
- The NEW reader returns the screenshot path in well under 3 s in every mode.

The real app against the same owner: `node e2e/run.js clipboard-x11`. It shows WebKitGTK's own paste was the freeze: WebKit asks every owner for targets it never offered and waits ~30 s on each one.
