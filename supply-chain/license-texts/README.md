Standard licence texts from SPDX's license-list-data (`text/<id>.txt`), one per
identifier on the allow-list (deny.toml `[licenses]` + `../js-licenses.json`).

`scripts/oss-inventory.mjs` uses them only for a package that ships no licence
file of its own, so the in-app "Open source licenses" page still carries the
licence's text. Adding a licence to an allow-list means adding its text here:
the generator fails when a package ships no licence file and none of the
identifiers in its licence expression has a text in this directory.
