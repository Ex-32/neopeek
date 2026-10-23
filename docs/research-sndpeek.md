# sndpeek 1.41 — reference analysis for neopeek (clean-room notes)

Source examined: `/tmp/sndpeek-1.41` (from
`soundlab.cs.princeton.edu/software/sndpeek/files/sndpeek-1.41.tgz`,
GPLv2-or-later tarball, ~312 KB). All claims below cite paths under
`src/`. This is a **written analysis only** — no code was copied. It is a
reference for reimplementing the *behavior* of sndpeek from scratch in Rust.

**Bottom line for the clone:** audio in is a duplex 2ch stream at 48 kHz
(Linux default), grabbed in 1024-sample frames; a 1024-sample Hanning
window is zero-padded to a 2048-point FFT for the spectrum view; 48
slices of log-spaced magnitude spectrum are kept in a ring and drawn as a
3D "waterfall" along the z-axis (~1 second of history); plus a 2D
waveform, a Lissajous figure, and (optional, marked *broken*) feature
overlays. Everything in sndpeek ships under GPL v2-or-later, so the
"clean-room" bar is: reimplement fresh, do not copy expression.

---

## Overview

- **What it is:** a real-time audio visualization tool ("animated, 3D"),
  origin 2003 at Princeton Sound Lab (Ge Wang, Perry R. Cook, Ananya
  Misra). Homepage/README: `README`. Renderer is legacy OpenGL fixed
  pipeline via GLUT.
- **Package layout:**
  - `src/sndpeek/` — the app: `sndpeek.cpp` (1947 lines, everything),
    `sndpeek.h`, `chuck_fft.c` (FFT), `RtAudio.cpp/.h` (audio I/O),
    `Stk.cpp/.h` (sample-rate holder), `Thread.cpp/.h` (mutex),
    `util_sndfile.c` (libsndfile shim, 34k lines), plus per-platform
    `makefile.{alsa,jack,oss,osx,win32}`.
  - `src/marsyas/` — vendored Marsyas feature-extraction modules
    (Centroid, DownSampler, Flux, LPC, MFCC, RMS, Rolloff, NormRMS, etc.)
  - Default build is a dispatcher: `make linux-alsa`, `make linux-jack`,
    `make linux-oss`, `make osx`, `make win32` (`src/sndpeek/makefile`).

## Audio capture

- **Abstraction:** bundled **RtAudio 4.1.1** (`src/sndpeek/RtAudio.h`,
  `#define RTAUDIO_VERSION "4.1.1"`), the classic pre-ASIO-era duplex API.
  Compiled backends are chosen by `#define` and let the underlying device
  API do the work:
  - ALSA: `makefile.alsa` defines `__LINUX_ALSA__`; impl in `RtAudio.cpp`
    (lines 6729+) uses ALSA 1.x (`snd_pcm_open`, `snd_pcm_hw_params_...`,
    default 4 periods).
  - JACK: `makefile.jack` defines `__LINUX_JACK__`; also links `-lasound`.
  - OSS: `makefile.oss` defines `__LINUX_OSS__`.
  - OS X: `__MACOSX_CORE__` (CoreAudio); Windows: DirectSound.
- **Format/config** (`sndpeek.cpp` top):
  - `SAMPLE = float`, `RTAUDIO_FORMAT = RTAUDIO_FLOAT32` (normalized ±1.0).
  - `NUM_CHANNELS = 2`, `SND_BUFFER_SIZE = 1024` (frames), capture is
    **duplex**: 2ch in from mic, 2ch out monitors input.
  - **Sample rate default: 48000 on Linux** (`__LINUX_ALSA__/OSS/JACK`),
    **44100 elsewhere**; overridable with `--srate:`; for file input the
    file's rate wins.
  - Buffer = 1024 frames ⇒ ~21.3 ms @48k. RtAudio is asked for exactly
    `g_buffer_size` frames; it warns if the device picks a different size.
- **Callback flow** (`cb()` in `sndpeek.cpp`): stereo→mono by averaging
  L+R into `g_audio_buffer` (or mono→stereo copy). A `Mutex g_mutex` +
  `GLboolean g_ready` hand off each buffer to the render thread, which
  polls `usleep(1000)` until `g_ready` is set. This is a callback-based
  pull, not a push — the GLUT `idleFunc` just calls `glutPostRedisplay`.
- **File input:** via bundled `util_sndfile.c` (libsndfile-compatible
  shim; `src/sndpeek/util_sndfile.c`) or native `-lsndfile`
  (`__USE_SNDFILE_NATIVE__`). Supports wav/snd/aiff/mat/raw. File playback
  reuses the same 1024-frame callback path.

## DSP pipeline

- **Windowing** (`initialize_audio`, `chuck_fft.c`): a **Hanning** window
  `hanning(g_window, g_buffer_size)` over the full 1024-frame buffer; also
  provides `hamming`/`blackman` (unused). `apply_window` multiplies
  element-wise.
- **FFT** (`chuck_fft.c`, adapted from the CARL CMusic distribution):
  custom power-of-two `rfft`/`cfft` with bit-reversal (Danielson–Lanczos
  recursion), forward transform scaled by `1/(2N)`. `N MUST be a power of
  2`. There are **two FFT sizes** because the display and feature paths
  differ:
  - **Spectrum/waterfall path (display):** `g_fft_buffer[SND_FFT_SIZE]`
    = 2048 floats are zeroed, first 1024 audio samples copied in, then
    `rfft(buffer, g_fft_size/2=1024, FFT_FORWARD)` ⇒ a **2048-point FFT**
    (1024 audio samples zero-padded to 2048), producing **1024 complex
    magnitude bins** (`SND_FFT_SIZE/2`), stored per waterfall slice.
  - **Feature path (`extract_buffer`, headless `--nodisplay`):** local
    1024-sample buffer, `rfft(buffer, g_buffer_size/2=512, FWD)` ⇒
    **1024-point FFT**, 512 magnitude bins fed to Marsyas (feeding size
    `SND_MARSYAS_SIZE = 512` exactly).
- **Magnitude → y mapping** (`displayFunc`): linear
  `y = gain * freq_scale * 1.8 * pow(25*|c|, 0.5)` or dB
  `y = gain * freq_scale * (20*log10(|c|/8)+80)/80` (toggle 'd').
- **Frequency-axis display mapping:** *log spacing* — `map_log_spacing`
  `= pow(ratio, power) * fft_size / g_freq_view`, precomputed for 1024
  bins in `g_log_positions`; `g_log_factor` default `1.0` = linear
  display; keys 'c'/'v' scale the log factor. Only ~half the x-range is
  visually meaningful because bins are squashed toward the right.
  `g_freq_view` defaults to 2 (full 1024-bin sweep drawn).
- **Features (Marsyas, `src/marsyas/`)** — each is a small math module
  computed off the magnitude spectrum; the app marks the view *"(broken)"*:
  - `Centroid` (`Centroid.cpp`): `centroid = m1/m0` (first moment / zeroth
    moment) over bins; Hz = `centroid/512 * srate/2`.
  - `Flux` (`Flux.cpp`): spectral flux — RMS-normalized (via `NormRMS`)
    difference vs previous window, squared, summed, `*1000` display hack.
  - `RMS` (`RMS.cpp`): `sqrt(mean(x²))` over the magnitude window.
  - `Rolloff` (`Rolloff.cpp`): smallest bin index where cumulative sum
    crosses `perc * total`, run at **50% (`g_rolloff`)** and **80%
    (`g_rolloff2`)**.
  - `LPC` (`LPC.cpp`, order **30** from `defs.h DEFAULT_ORDER`),
    `MFCC` (`MFCC.cpp`: **13** cepstral coefs, 13 linear + 27 log mel
    filters, lowest freq 133.33 Hz, log spacing 1.0711703) — allocated and
    only used in the headless `extract_buffer` stdout dump, **not drawn**.
  - `DownSampler` (factor 2) allocated but unused in the hot path.
  - Display mode computes only centroid/flux/rms/rolloff/rolloff2, each
    smoothed by a 4-sample ring average (`LP = 4`).

## Rendering model

- **Context:** OpenGL 1.x fixed pipeline (`glBegin/glEnd`), GLUT double
  buffer + RGB + depth, `glClearColor(0,0,0,1)` (black bg), smooth
  shading, two lights, `gluPerspective(45°, aspect, 1, 300)` and a
  `gluLookAt(0, 3.5·sin(eye_y), 3.5·cos(eye_y), 0,0,0, …)` camera that
  **orbits the origin**. Default window 800×600.
- **Four things drawn on every frame** (all one big `glRotatef(g_angle_y,
  0,1,0)` about Y, so the whole scene spins together):
  1. **2D waveform** (`g_waveform`, key '1'), drawn at `y=0.7`, color
     `(0.4, 0.4, 1.0)` blue, `GL_LINE_STRIP` across
     `g_buffer_size / g_time_view` samples (default full 1024), x-span
     -1.8…1.8, vertical scaled by `gain · time_scale · 2`.
  2. **Lissajous figure** (`g_lissajous`, key '2'), drawn at `x=1.2`,
     color `(1.0, 1.0, 0.5)` yellow, `GL_LINE_STRIP`; stereo = L on X vs R
     on Y (scaled by `lissajous_scale=0.45`); mono = signal vs itself
     delayed by `g_delay` samples (default `SND_BUFFER_SIZE/2 = 512`),
     using `g_back_buffer` ring — a pseudo-Lissajous from a single channel.
  3. **3D waterfall / "wutrfall" spectrum** (`g_wutrfall`, keys '3'/'w'):
     the centerpiece.
  4. **Feature overlay** (`g_draw_features`, key '4'): red centroid
     vertical line + label "centroid = … Hz", RMS bar, 50%/80% rolloff
     markers, and a text HUD (see Interaction).
- **Waterfall storage & layout** (the numbers to reimplement):
  - `g_depth = 48` history slices; each slice is `Pt2D g_spectrums[i]`
    sized `[SND_FFT_SIZE] = 2048` (only 1024 bins used) — i.e.
    `48 × 2048 × 8 bytes ≈ 786 KB`.
  - Slices laid out along **z**, spaced `g_space = 0.12`, plus offset
    `g_z`. Ring index `g_wf` decrements each frame (mod 48). For file
    playback a delay of `g_wf_delay = round(depth/3) = 16` frames exists
    between the "now" slice and the persisted history; for live mic
    input `g_wf_delay = 0`.
  - Each slice drawn as `GL_LINE_STRIP` over `g_fft_size/g_freq_view`
    bins (default 1024 points), x from the log-spaced `g_log_positions`,
    y = stored magnitude, z = slice depth. Line width adjustable (`,`,
    `.'`), 1–4.
  - **Depth cueing colors:** past slices green-ish `(0.4·fval, 1.0·fval,
    0.4·fval)`, future slices rusty `(1.0·fval, 0.7·fval, 0.4·fval)`,
    where `fval = (depth - i + drift)/depth` dims older planes; optional
    **rainbow** mode ('r') blends hue by depth; the "now" line is cyan
    `(0.4, 1.0, 1.0)`.
  - Direction toggle 'b' goes forward/backward along z.
- **History depth in time:** 48 frames × 1024 samples ≈ **48 × 21.3 ms ≈
  1.02 s** of spectrum history @48k (~1.11 s @44.1k). This ≈1-second
  rolling 3D spectrogram is the signature sndpeek look.

## Interaction / controls

- **Camera:** not a free orbit. The camera is fixed `gluLookAt` above the
  origin; the **whole scene rotates continuously about Y** at `g_angle_y`.
  Left mouse button press nudges `g_angle_y` down (`g_inc -= 1.0`), right
  press nudges it up — i.e. click-to-spin-until-released, not drag.
  `[` / `]` adjust the eye's polar angle `g_eye_y` (up/down tilt) in
  `g_inc_val_kb = 0.025` steps. There is **no mouse drag-pan or wheel
  zoom**; "zoom" is a scene scale.
- **Scales (keys):** `-`/`=` spectrum (`freq_scale` ×0.99/×1.01);
  `_`/`+` waveform (`time_scale`); `l`/`L` Lissajous; `c`/`v` log factor
  (spectrum curvature); `u`/`i` waterfall slice spacing (`g_space`
  ×1.02/×0.98); `j`/`k` move waterfall +/− z (`g_dz=0.1`); `C`/`V`
  restrict time-domain window (`g_time_view` 1..32 → 1024..32 samples
  shown); `,`/`.` spectrum line width, `<`/`>` waveform line width;
  `y`/`Y` Lissajous delay ±10 (0..buffer size).
- **Coloring/sampling toggles:** `d` dB vs linear spectrum; `r` rainbow
  waterfall; `b` backward waterfall; `f`/`z` freeze frame (keeps glowing,
  still rotate/scales); `m` mute; `s` fullscreen; `1` wave, `2`
  lissajous, `3`/`w` waterfall, `4` features; `p` prints all settings to
  console; `x` restart file; `q` quit; `h` help (full table in `help()`
  in `sndpeek.cpp`).
- **HUD / readouts (`draw_string`, GLUT stroke font):**
  - Feature panel (top-left, when enabled): `centroid = … Hz`, `flux =
    …`, `RMS = …`, `50% rolloff= …`, `80% rolloff = …` (all
    lowpass-averaged over 4 frames).
  - With `--showtime` ('t'): elapsed seconds string.
  - `paused… (press f to resume)` and `muted… (press m to unmute)`
    overlays.
  - `--print`/`--stdout` mode dumps `centroid flux rms rolloff rolloff2`
    + 13 MFCC coefs as space-separated floats per frame to stdout.
- **Command-line options** (`usage()`): ON/OFF flags
  (`fullscreen|waveform|lissajous|waterfall|dB|features|fallcolors|
  backward|showtime|freeze`), numeric (`inputDevice|outputDevice|
  timescale|freqscale|lissscale|logfactor|spacing|zpos|dzpos|depth|
  preview|yview|rotatem|rotatek|begintime|ds`), and `srate`, `nodisplay`,
  `sndout`. Notable: `--depth:` sets slice count; `--nodisplay` runs
  headless feature extraction from mic/file; `--preview:` sets the
  waterfall delay ratio (0≤v<1).

## Licensing notes

- **sndpeek itself:** **GNU GPL version 2 (or, at your option, any later
  version)**. Every source header says so (`sndpeek.cpp`, `sndpeek.h`);
  `COPYING` is the verbatim GPLv2 (June 1991) text.
- **Bundled third-party components** (so the effective license of the
  whole tarball is GPL-family, with a permissive RtAudio exception):
  - `RtAudio.cpp/.h` — **MIT-style permissive** (Copyright Gary P.
    Scavone; header in `RtAudio.h` grants free use; asks only that
    modifications be offered back, non-binding).
  - `chuck_fft.c` — derived from the **CARL CMusic distribution**.
  - `src/marsyas/*` — **GPL** (each file: George Tzanetakis, GPL v2+).
  - `Stk.cpp/.h` (STK) — Perry Cook, its own free license, used only as a
    sample-rate holder in sndpeek.
  - `util_sndfile.c` — libsndfile shim (LGPL upstream).
- **What this means for neopeek (clean-room):** sndpeek is not permissive,
  so neopeek must be an **original implementation** — behavior/discrete
  parameters may be matched from reading the source (constants like 1024 /
  2048 / 48 / 0.12 are facts, not copyrightable expression), but no code,
  comments, or structure may be copied. The safe route is: write fresh
  Rust (own FFT via e.g. `rustfft`, own feature math), match the *values*
  above, and license neopeek independently. Do not vendor or port the
  Marsyas/CARL/RtAudio sources.

## Lessons for a modern rewrite

1. **Air of the design:** one duplex audio thread pushes 1024-sample mono
   frames; a render thread pulls the latest frame, windows it, FFTs it,
   and appends the magnitude spectrum to a bounded ring of z-laid slices.
   Keep that clean producer–consumer split (Rust: an audio callback /
   channel pushing into a lock-protected ring, renderer consuming).
2. **Concrete constants to honor** (all verified in `sndpeek.cpp`):
   `buffer = 1024`, `FFT = 2048` (zero-pad) for display, `48` slices,
   `z-spacing = 0.12`, `log factor default 1.0`, linear mag
   `sqrt(25·|X|)` (or dB variant), Hanning window, default 48000 Hz,
   3 views (waveform / lissajous / 3D waterfall) + feature HUD.
3. **Modernize the FFT:** 1024-pt zero-pad-to-2048 is cheap; `rustfft`
   gives better numerics than the CARL radix-2 with no effort. Recompute
   slice magnitudes on a fixed cadence independent of GL vsync.
4. **Camera:** the always-spinning autonomous Y-rotation is charming but
   arbitrary — expose an orbital camera (yaw/pitch/zoom) in the rewrite,
   defaulting to the sndpeek look. Map mouse wheel to true camera dolly
   (sndpeek never had one) and keep per-view scale multipliers.
5. **Log-frequency axis** is the one thing that visually looks "off" at the
   default linear factor — consider making the default mildly logarithmic
   (`log factor ≈ 1…2`) with a linear toggle, since the linear default
   wastefully squashes bins. Preserve the `g_log_positions` precompute
   idea (one float array, reused every slice).
6. **Feature "broken" panel:** centroid/flux/RMS/rolloff are easy and
   faithful to reproduce; skip or clearly mark LPC/MFCC (only existed for
   headless stdout). A 4-frame moving average on the HUD reads well.
7. **Audio is the tricky port:** RtAudio 4.1.1 (ALSA/JACK/OSS) is old and
   its callback model predates modern ergonomics — in Rust use
   `cpal` (ALSA/CoreAudio/WASAPI) or `jack` crate with an explicitly sized
   buffer; request 1024 frames but clamp to the device's actual size the
   way RtAudio warns about. Duplex monitor (play the captured input back)
   is optional; sndpeek defaults to it, so decide whether neopeek mutes by
   default.
8. **No code reuse under GPL:** reimplement all DSP and interaction from
   the constants/behavior above; do not port Marsyas/RtAudio/CARL code.