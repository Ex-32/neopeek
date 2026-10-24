# neopeek

A modern, clean-room reimplementation of [sndpeek](https://github.com/guscreek/sndpeek)'s
3D FFT-waterfall visualizer: live audio → real-time 3D spectrum waterfall,
waveform, Lissajous figure, metering and diagnostics, rendered with wgpu
(Vulkan/GL fallback) — no X11/GLUT, no GPL code.

> sndpeek is GPL; neopeek is a **clean-room** project: behavior-matched from
> recorded constants and observation (see `docs/research-sndpeek.md`), no
> code ported. Licensed MIT OR Apache-2.0 (see `LICENSE-MIT` /
> `LICENSE-APACHE`).

![screenshot placeholder — add a screenshot of the running waterfall](docs/screenshot.png)

## Build & run

```sh
nix build .#          # release binary → ./result/bin/neopeek
nix run .#            # run directly (wrapProgram handles Vulkan/GL loader paths)
nix develop .#        # dev shell (cargo, clippy, libpulse, alsa, vulkan-loader)
```

Plain `cargo build` works too when `libpulse`/`alsa-lib` are linkable in the
environment; under NixOS use the dev shell (`nix develop .# --command cargo …`).

Runtime needs: a Vulkan (or GL) driver and a PulseAudio-compatible capture
server — PipeWire hosts work transparently via `pipewire-pulse`.

## Controls

Mouse: left-drag orbit · shift-/middle-drag pan · wheel zoom.

| key | action |
|-----|--------|
| `1` / `2` / `3` (or `w`) | waveform / Lissajous / waterfall toggle |
| `S` | filled-surface waterfall mode (theme `surface_ramp` colors) |
| `d` | dB magnitude mode (theme `db_floor`/`db_ceil` window) |
| `r` | rainbow depth colors |
| `u` / `i` | slice z-spacing wider/narrower |
| `j` / `k` | waterfall z-offset toward/away |
| `-` / `=` | spectrum frequency axis ×0.99 / ×1.01 |
| `_` / `+` | waveform vertical scale |
| `l` / `L` | Lissajous figure scale |
| `<` / `>` | waveform/Lissajous strip width (world-space quads) |
| `y` | pseudo-Lissajous delay (mono-ish sources): 512 → 256 → 128 → 64 → 512 samples. **Note:** `Y` is the snap-side camera view, not this. |
| `[` / `]` | camera tilt steps |
| `f` (or `z`) | freeze — stop ingesting frames, keep rendering |
| `m` | mute — also stops frame ingestion (listener-only build) |
| `h` | keybind-hint line toggle |
| `H` | whole HUD toggle |
| `T` | cycle themes (Ember → Ice → Terminal → loaded custom) |
| `F` | camera reset/fit |
| `X` / `Y` / `Z` | snap views: top / side / front |
| `p` | print all live settings to the console (sndpeek parity) |
| `q` / `Esc` | quit |

The HUD: left = L/R RMS meters with 800 ms peak-hold ticks + feature panel
(spectral centroid in Hz, flux, RMS, 50%/80% rolloff); right = fps, frame ms,
GPU adapter; bottom = device, backend, rate, FFT/window/hop, history depth,
keybind hints; center = frozen/muted overlays.

## Themes

RON files (PLAN §6.1): background, waterfall colors (`past`/`future`/
`now_line`, optional `rainbow`, dB window, optional `surface_ramp` stops),
waveform/lissajous color+scale, meter gradient + peak hold, HUD text colors
and font size. Example — see `crates/neopeek-theme/assets/themes/Ember.ron`.

```ron
(
    name: "Ember",
    background: "#0b0b10",
    waterfall: ( past: "#66ff66", future: "#ffb366", now_line: "#66ffff",
                 rainbow: false, db_floor: -80.0, db_ceil: 0.0 ),
    waveform: ( color: "#6666ff", width: 1.0 ),
    lissajous: ( color: "#ffff80", scale: 0.45 ),
    meters: ( grad_low: "#59ff4d", grad_high: "#ff4d4d", peak_hold_ms: 800 ),
    hud: ( text: "#cfd3dc", dim: "#6b7280", font_size: 14.0, accent: "#e2542a" ),
)
```

Colors are `#rgb`, `#rrggbb` or `#rrggbbaa`.

Built-ins: **Ember** (default — the original sndpeek-family palette), **Ice**
(blue/cyan), **Terminal** (phosphor green on near-black).

`--theme <name|path>`: an existing `.ron` path loads that file; otherwise
`$XDG_CONFIG_HOME/neopeek/themes/<name>.ron` is tried; otherwise a built-in
name; otherwise a warning is logged and Ember is used. A malformed or missing
file never crashes the app. `T` cycles the built-ins plus a loaded custom
theme; `--watch-theme` re-parses the loaded file on every change (dev aid,
off by default).

## Audio

- Backend: PulseAudio protocol (`--backend pulse`, the default and only
  backend for now) — covers PipeWire hosts via `pipewire-pulse`. An ALSA
  fallback (`cpal`) is planned.
- `--list-devices`: enumerate sources (mics + sink monitors, `monitor`
  flagged).
- `--device <name>`: record from a specific source; default is the default
  sink's monitor ("what the host hears").
- `--dump-pcm [seconds]` / `--dump-fft [seconds]`: stdout dumps of raw
  interleaved f32 PCM / verbose per-slice features (capture must be
  available).
- `--print [seconds]`: sndpeek parity — one space-separated line per
  FFT slice: `centroid_hz flux rms ro50_hz ro80_hz` (plain floats;
  no MFCC). Shares the capture+FFT path with `--dump-fft`, which prints
  the more verbose debug format instead.
- The negotiated sample spec (rate/format/channels) is returned by the
  capture handshake and refused if not f32 stereo; the DSP window is 1024
  frames (≈21.3 ms @48 kHz) zero-padded to a 2048-point FFT (1024 bins).

## Architecture

Workspace crates (PLAN §2.1):

- `neopeek-audio` — PulseAudio capture, PCM SPSC ring, device listing
- `neopeek-dsp` — windowing + FFT engine, fixed-size POD `Frame` (bins,
  mono, L/R, RMS/peak, spectral features)
- `neopeek-render` — wgpu pipelines (waterfall line + filled-surface,
  waveform/Lissajous quad strips, glyphon HUD), camera, frame ring
- `neopeek-theme` — RON theme model, built-ins, `--theme` resolution,
  hot-reload watcher
- `neopeek-config` — CLI surface
- `neopeek` — winit app shell, input mapping, HUD assembly, DSP thread

Hot paths (capture → DSP → frame-drain) are allocation-free with fixed-size
POD `Frame`s and a lock-free SPSC ring; drop-oldest at the consumer. The HUD
assembly allocates small per-frame strings for text shaping (render-side
only). The active `Theme` is copied once per redraw (a handful of short
strings — negligible, and it keeps borrows disjoint).

## License

MIT OR Apache-2.0. The bundled JetBrains Mono font
(`crates/neopeek-render/assets/fonts/`) is under the SIL Open Font License
1.1 (`JetBrainsMono-OFL.txt` alongside it). All sndpeek knowledge here is
clean-room observed behavior, not ported code.
