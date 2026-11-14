# Remote control

Use your phone or tablet as the pedal's screen. The pedal serves a small web
app; open it in any recent browser on the same network. It has two modes:

- **Editor**: the pedal's own UI, the same screens as on a touchscreen
  (rack, presets, store, settings…), drawn on the pedal and streamed to the
  browser. Taps, drags, scrolling and typing go back to the pedal. This is
  the main mode, and the only screen of a pedal built without a touchscreen.
  It needs a landscape view of at least 800 × 480 CSS pixels: a tablet, or a
  phone turned sideways.
- **Live**: a portrait page made for the stage: preset and scene switching,
  setlist slots, tap tempo, the tuner, the meters, and every block with its
  bypass and its controls.

A **Status** page shows the version, the pedal's addresses, the hotspot and
the devices signed in, and signs devices out.

The button at the right of the tabs puts the page in full screen (no
address bar, no system bars) and back, where the browser allows it: Android
browsers, desktop browsers and iPad. iPhone Safari has no full screen for
pages, so the button is not shown there.

The remote control is off until a password is set. Nothing of it runs while
it is off: no web server, no timers, no work in the UI's refresh.

## Set-up

### Without a screen

Edit `pedalboard.txt` on the card's FAT partition (from any computer) before
the first start:

```ini
remote_password=choose-8-to-63-characters
# optional: a Wi-Fi network to join
wifi_ssid=Home
wifi_password=its-key
```

At the next start the pedal keeps the password as a hash and the Wi-Fi
network as a NetworkManager profile on its data partition, then rewrites
those lines as `remote_password=(saved)`, `wifi_ssid=(saved) Home` and
`wifi_password=(saved)`: the card holds no secret afterwards. Write a new
value to change it, or `remote_password=off` to turn the remote control off
(the password, the hotspot and every session go).

Then, from the phone or tablet:

- on the pedal's hotspot `OpenRig-XXXX` (key: the remote control password),
  open <http://10.42.0.1>;
- on your own network, open `http://<the pedal's address>` (your router
  lists it as `pedalboard`). With Network MIDI on, avahi also announces the
  pedal's name: `http://pedalboard.local` works too.

### With a touchscreen

Settings → Remote control: turn it on, choose the password, and read the
addresses to open. The same card changes the password, signs every device
out, and sets the hotspot.

## Where the screen is

`display` in `pedalboard.txt`:

| Value | |
|---|---|
| `auto` (default) | The touchscreen. With a remote control password set and no touchscreen (no display, or a display without touch input, such as a plain HDMI monitor), the UI is for the remote control |
| `screen` | Always the touchscreen |
| `remote` | Always the remote control |

In `auto`, with the remote control on and a display connected, the app waits
up to 10 s for its touch input (some panels bring it up late) before
deciding; the log says what it found:

```
No touchscreen: the UI is for the web remote reason=a display, no touch input
```

Without a touchscreen, the display (if any) keeps the boot logo. Without a
password, nothing changes: the UI is on the display as before, and the log
says to set `remote_password`.

## Hotspot

`remote_hotspot` in `pedalboard.txt` (also in Settings → Remote control):

| Value | |
|---|---|
| `auto` (default) | Started when the pedal has no Ethernet or Wi-Fi connection 45 s after starting, then kept |
| `on` | Always: the Wi-Fi card is the hotspot, it joins no network then |
| `off` | Never |

The network is `OpenRig-XXXX` (the end of the Wi-Fi card's address), WPA2
(AES), its key the remote control password; the pedal is `10.42.0.1` and
gives addresses to the devices that join (NetworkManager's shared mode with
dnsmasq). It reaches nothing else: no Internet through the pedal. Wi-Fi
cards that cannot be an access point (some x86 ones) say so in Settings and
on the Status page; the remote control still works over Ethernet or Wi-Fi.

## Security

What it protects against: someone on the same network, or a web page open
in the phone's browser, taking control of the pedal or reading the
password.

- Off by default. The password is 8 to 63 characters, kept as an Argon2id
  hash (`remote.json` in the data directory, mode 0600). It is not written
  in any log, and leaves the card at the first start.
- Signing in gives a random 256-bit session in an `HttpOnly`,
  `SameSite=Strict` cookie. Sessions live in memory only: a restart, a new
  password, Turn off or "Sign out all devices" end them all. 32 at most;
  unused for 30 days, one ends.
- Wrong passwords slow down: three are free, then 1 s, 2 s, 4 s… up to
  5 min before the next try, for everyone (a single pedal, few users). One
  password check runs at a time.
- Only clients with a private, link-local or loopback address are served
  (10/8, 172.16/12, 192.168/16, 169.254/16, 127/8, fc00::/7, fe80::/10).
- The `Host` header must be an IP address or one of the pedal's names
  (`openrig.local`, `openrig`, its host name with and without `.local`,
  `localhost`): another site's name resolving to the pedal is refused (DNS
  rebinding). Changes and the WebSocket need an `Origin` of the same host
  (no cross-site requests).
- Backups (Status page): `GET /api/backup` and `POST /api/restore` need a
  session; the upload also needs the same `Origin`. A backup never holds
  the password, and an uploaded one is checked in full before the pedal
  restarts to restore it. See [BACKUP.md](BACKUP.md).
- Strict Content-Security-Policy (`default-src 'self'`, no inline script),
  `X-Frame-Options: DENY`, `nosniff`, no referrer, no caching.

Limits:

- The hotspot's NetworkManager profile holds its key (the password) in
  clear, readable by root only, as for any saved Wi-Fi network.
- Plain HTTP. On the pedal's hotspot the WPA2 key encrypts the traffic; on
  someone else's Wi-Fi (a venue, a hotel), other devices on that network
  can see what the pedal and your phone exchange, the password included
  when you sign in. Use the hotspot or a network you trust. The app says so.
- Anyone with the password controls the whole pedal, settings included.
- The hotspot key is the password: share the hotspot, share the control.

## How it works

- A Rust web server (axum, on a thread of its own with a single-threaded
  tokio runtime) on port 80. The OS launcher grants the app only
  `CAP_NET_BIND_SERVICE` for it; without it the server falls back to 8080.
  `PEDALBOARD_REMOTE_PORT` sets another port.
- The page is static HTML, CSS and JavaScript built into the binary (no
  build step, no framework). One WebSocket carries everything: the state as
  JSON (sent when it changes, at most 4 times a second), the meters and tuner
  at 10 Hz while a client is connected (the tuner only runs while a client
  shows it), commands back, and the Editor's frames.
- Commands run on the UI thread through the same code as the touchscreen and
  MIDI: loading a preset, a scene, a setlist slot, bypass, a control, tap
  tempo. Two writers to the same control: the last one wins, except that a
  control held on the touchscreen ignores the remote for 300 ms.
- Editor: without a touchscreen, the app uses a Slint platform of its own: a
  window drawn in memory by the software renderer, at the size the browser
  asks for (800 × 480 to 1920 × 1200; one window for all clients, the last
  resize wins). Nothing is drawn while no client shows the Editor; then at
  most about 30 frames per second, and only the areas that changed. After
  a tap, drag or key, the frame showing its effect is sent as soon as the
  UI has handled it instead of waiting for the next slot. Small areas go as
  PNG, larger ones as JPEG (quality 80). `PEDALBOARD_REMOTE_FRAMES=png` or
  `jpeg` forces one. A frame is a 14-byte header (format, position, window
  size) and the picture; pointer and key events come back as JSON.
- Pictures are encoded on a thread of their own: the UI thread only draws
  and copies the changed areas. While a frame is being encoded, the UI keeps
  drawing and the next frame takes everything that changed meanwhile.
- Flow control: the page acknowledges each frame once drawn. At most 32 KiB
  of frames are on the way to a page; past that, frames are held back and
  their area is sent again, as it is then, once the page caught up. A slow
  link (weak Wi-Fi) or a slow phone gets fewer frames, never late ones: the
  delay stays under about 32 KiB over the link's speed instead of growing.
- The sockets send small messages at once (`TCP_NODELAY`): a knob or a
  meter does not wait for the acknowledgement of the previous frame.

Cargo features: `remote` (the server and the Live view) and `headless` (the
Editor's renderer), both on by default and in the OS image.

| Variable | Default | Purpose |
|---|---|---|
| `PEDALBOARD_DISPLAY` | `auto` | `auto`, `screen` or `remote` (set from `display` by the OS launcher) |
| `PEDALBOARD_REMOTE_PORT` | `80`, then `8080` | Port of the web server |
| `PEDALBOARD_REMOTE_FRAMES` | `auto` | `png` or `jpeg`: one format for every frame |
| `PEDALBOARD_REMOTE_STATUS` | unset | File where the app writes its remote control status (read by `pedalboard-report`) |

## Measures

x86_64 image in QEMU (4 cores, KVM, JACK 1024 × 3), release build, CPU of
the app process (100 % = one core):

| | CPU |
|---|---|
| Remote control off, UI on a 1280 × 800 display | 6.3 % |
| On, UI on the same display, no client | 6.2 % |
| On, no display, no client | 1.2 % |
| Live view open | 1.4 % |
| Live view, tuner on | 1.4 % |
| Editor open, nothing moving | 2.3 % |
| Editor, tapping and dragging | 2.9 % |

The Editor sends 10 to 15 KiB per changed area; a still screen sends only
what moves (the meters, the DSP load: about 2 KiB a second).

Delay of the Editor (WebSocket ping behind the frames, x86_64 desktop,
1280 × 800, a page change every 0.5 s or a knob dragged; the link is a
reader limited to a given speed):

| | Before | After |
|---|---|---|
| Fast link, page changes, 95th percentile | 41 ms | 0.4 ms |
| 150 KB/s, knob dragged, median | 5.5 s, growing | 0.21 s |
| 60 KB/s, page changes, median | 2.6 s, growing | 0.27 s |

## Troubleshooting

- `pedalboard-report` has a "Remote control" section: the app's status, the
  hotspot's, whether a password is set, and the hotspot's log.
- "Remote control: listening on port 8080": the app did not get port 80; open
  `http://<address>:8080`.
- The page says "Signed out": the password changed or the devices were
  signed out; sign in again.
