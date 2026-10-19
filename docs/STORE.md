# Plugin store

The Store page (Add block → STORE) installs LV2 plugins from the OpenRig
catalog: a signed index of free plugins, built for the board and checked
to load in OpenRig. It is built with the `store` feature (on by default).

## Charter

- **Free software only.** Every package in the catalog is under a free
  license, and its source is linked from the index.
- **Sideloading stays open.** Any LV2 bundle placed in `<data>/lv2` is
  loaded, whether or not it comes from the store. The store never touches a
  bundle it did not install.
- **No account, no tracking.** The device downloads the index and the
  archives over HTTPS from GitHub releases; it calls no API and sends
  nothing about the user.

## Where things are

`<data>` is `$PEDALBOARD_DATA_DIR`, else the platform data directory
(`~/.local/share/pedalboard` on Linux, `/data/pedalboard` on the OS image).

| Path | What |
|------|------|
| `<data>/lv2/<bundle>.lv2/` | Installed bundles; also where bundles are copied by hand |
| `<data>/lv2/<bundle>.lv2/.openrig.json` | Receipt of a store install: package, version, archive sha256, plugins |
| `<data>/store/catalog-v1.json`, `.minisig` | Last index accepted, with its signature |
| `<data>/store/cache/<sha256>.<format>` | Archives of the packages of the current index |
| `<data>/store/media/<id>.png` | Pictures of the entries |
| `<data>/store/trusted-keys.pub` | Extra public keys trusted for the index (one per line) |

`<data>/lv2` is loaded even when it is not in `LV2_PATH`; when `LV2_PATH`
already lists it (the OS image), it is not loaded twice.

## The index

The index is `catalog-v1.json` with its minisign signature
`catalog-v1.json.minisig`, on the `catalog` release of
[openrig-catalog](https://github.com/Siryu6/openrig-catalog), where its
format is described. `OPENRIG_CATALOG_URL` points the app at another index
(then plain HTTP is allowed too, for local tests).

An index is accepted when:

- its signature is a prehashed minisign signature (`ED`) by a trusted key:
  the keys built into the app (`CATALOG_KEYS` in `src/store/verify.rs`) or
  those of `trusted-keys.pub`;
- its schema is 1, its names, bundle folders and archive roots are plain
  relative names;
- its `serial` is not lower than the serial of the index already accepted
  (an old index cannot be replayed).

An index past its `expires` date is still shown, but installs are refused
until a fresh one is downloaded. The index is downloaded when the page opens
and none is cached, or the cached one is stale or more than 6 hours old, and
from the refresh button.

## Statuses

| Status | When | Actions |
|--------|------|---------|
| Available | Not on the board | Install |
| Installed | Installed by the store, same version as the index | Add to the chain, Remove |
| Update | Installed by the store, another version | Update, Remove |
| On board / Installed by hand | The plugin URI is loaded but the bundle has no receipt (system plugin or copied by hand) | Add to the chain |
| Unavailable | No build for this architecture, or the plugin needs a host feature OpenRig lacks | — |

## Install

1. The archive for the board (`aarch64` or `x86_64`) is taken from the
   cache, else downloaded to a `.part` file and checked against the size and
   sha256 of the index.
2. It is unpacked (`tar.gz` or `tar.xz`) into a staging folder of
   `<data>/lv2`: only regular files and folders under `<root>/<bundle>/` are
   kept; links, devices, absolute paths and `..` are refused, and so is a
   bundle over 512 MB.
3. The receipt is written, then the staging folder is renamed into place;
   an update moves the old bundle aside first and deletes it once the new one
   is in. A bundle folder that exists without a receipt is never replaced.
4. The plugins are scanned again: the new ones can be added at once, without
   restarting.

Every entry of the index sharing the bundle is installed together, and
listed in the receipt.

## Removal

Only bundles with a receipt can be removed. The bundle is renamed aside,
deleted, then the plugins are scanned again. Blocks already in the chain
keep running until the chain is rebuilt; presets using the plugin keep its
settings (by port symbol), and get them back when it is installed again.

The libraries of earlier scans are kept loaded until the app exits, so a
running block never loses its code.

## Plugins copied by hand

Copy the bundle into `<data>/lv2`, then press the refresh button of the
Store page: the folder is scanned again with the index (or restart the app).
These bundles show as *Installed by hand* when the catalog lists them.

## Tests

- `cargo test --features store --lib store`: index, signature, unpacking
  and install rules.
- `tests/store_test.rs`: end to end without network, with a throwaway key,
  a compiled test plugin, install, rescan, run and removal.
