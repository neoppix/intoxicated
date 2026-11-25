<p align="center">
  <img src="packaging/branding/intoxicated-logo.png" alt="Intoxicated" width="200">
</p>

<h1 align="center">Intoxicated</h1>

<p align="center">
  <strong>Roblox on FreeBSD. 100% native.</strong><br>
  No linuxulator. No emulator. No Wine. No Waydroid. Nothing under <code>/compat/linux</code>.<br>
  The official Roblox engine running as a real FreeBSD process.
</p>

<p align="center">
  Bugs and feature requests go on
  <a href="https://github.com/neoppix/intoxicated/issues">this repo's issues</a>.
</p>

<p align="center"><em>A hobby project, not a commercial one. Please don't DMCA it.</em></p>

## What it is

Intoxicated loads Roblox's official **Android x86-64** engine directly on FreeBSD
through its own runtime: the AOSP bionic linker, a bionic-to-FreeBSD libc shim, a
JNI VM (libjnivm) in place of Android's, and a framework layer that answers the
engine's calls into Android.

**It runs native, all the way down.** The `intoxicated-run` binary is a native
FreeBSD ELF, not a linuxulator program. The graphics go through native FreeBSD
Vulkan or GLES2. The input goes through native X11. And `libroblox.so` itself, the
Linux/Android engine, is loaded and executed by the AOSP bionic linker as
native FreeBSD code, with FreeBSD-specific shims wherever the two ABIs disagree
(syscalls, `pthread`, `environ`, `/proc`, futexes). Nothing emulates Roblox and
nothing runs it under the Linux ABI. It talks to your GPU the way any native app
does.

The headline port work: Roblox's anticheat kicks for **reason 304** on FreeBSD,
because `libroblox.so` makes raw kernel-ABI syscalls the bionic setup can't route
cleanly. Intoxicated patches every raw syscall site in the engine to `ud2` (16 in
Roblox 2.748.1527; the sites are found automatically at load by disassembling the
library, and cached per build in `~/.cache/intoxicated/syscall-sites/`) and catches
the resulting `SIGILL`, routing each through `bionic_syscall`, so the anticheat
stops seeing the thing it was killing the client for. That fix is what makes any
of the rest matter.

On top of that it can draw its own [visual effects](#visual-effects) under the
HUD: reflections, depth of field, bloom, lens effects and sharpening, with a
competitive and a cinematic mode, all optional.

## Get it running

This is a **build-from-source hobby port** on FreeBSD, tested on FreeBSD
14.4-RELEASE with an NVIDIA GPU (RTX 40-series), native X11, and native Vulkan.

**Zero `/compat/linux` at runtime.** The engine reads Linux `/proc` and `/sys`;
every one of those reads (`meminfo`, `cpuinfo`, the process list the anticheat
scans, `self/{fd,auxv,status,maps}`, and the rest) is answered from FreeBSD's own
`sysctl`/`elf_aux_info`, not from linprocfs. There is no linuxulator and no
Linux-compat filesystem mount to set up. Verified with `ktrace` over a full
session (sign-in, join, deploy, play): **0** of 52,352 path lookups were under
`/compat/linux`.

**Quick install.** Paste this into a terminal, as your own user:

```sh
fetch -qo - https://raw.githubusercontent.com/neoppix/intoxicated/freebsd-port/install.sh | sh
```

It checks what the system is missing (packages, the GPU driver and its kernel
module), lists the commands that need root and asks before running them, with
the password going to sudo, doas or su. Then it clones the checkout to
`~/intoxicated` (`INTOXICATED_REPO` picks another place), builds it, installs
the launcher and the desktop entry, offers
[Intoxicated Settings](#intoxicated-settings), the
[visual effects](#visual-effects) and to open roblox.com's Play button in
Intoxicated, and fetches the Roblox build. Running it again, or
`intoxicated --update`, updates an install. It does what the steps below do,
which stay here for anyone who would rather do them by hand.

The visual effects (screen-space reflections, depth of field, bloom, lens and
sharpening) are optional and asked about after Intoxicated Settings, no by
default; even when installed they stay off until you turn them on. The answer
is kept in `~/.config/intoxicated/install.conf`, so `intoxicated --update`
builds the same way and does not ask again. To change it later:

```sh
intoxicated --update --effects      # add them (or: sh install.sh --effects)
intoxicated --update --no-effects   # take them out again
```

Without them the client is built with `cargo build --release
--no-default-features`: none of their code or shaders is in it, their
`INTOXICATED_*` variables are ignored (the log says so once), their devctl
verbs answer `err not built`, and the Shaders page of Intoxicated Settings
says how to add them. An install from before this choice keeps its effects.

With the visual effects, the installer then asks about the
[experimental ray tracing](#experimental-ray-traced-reflections), no by
default (`INTOXICATED_YES=1` does not say yes to it); it is kept as `RT=` in
the same file. Built in, it still stays off until turned on:

```sh
intoxicated --update --rt           # add it (needs the effects; or: sh install.sh --rt)
intoxicated --update --no-rt        # take it out again
```

With it the client is built with `cargo build --release --features rt`;
without it none of its code or shaders is in the client, `INTOXICATED_RT=1`
is ignored (the log says so once) and devctl `rt` answers `err not built`.

**1. Prerequisites** (as root):

```sh
# Build tools, native X11, native Vulkan loader, epoll for the engine's event
# loop, and jq (the launcher edits flags.json)
pkg install git rust cmake pkgconf libepoll-shim libX11 libXi libXinerama libXrandr libxkbcommon vulkan-loader jq

# Audio needs no packages: the client writes to OSS (/dev/dsp) directly, and
# sound(4) is part of base. It plays to /dev/dsp, which follows
# hw.snd.default_unit; set INTOXICATED_AUDIO_DEVICE=/dev/dspN to pick another.

# Native GPU driver (this is what provides the real Vulkan ICD)
pkg install nvidia-driver          # NVIDIA; use the Mesa drivers for AMD/Intel
sysrc kld_list+="nvidia-modeset" && kldload nvidia-modeset
```

You also need a running X11 session. That is the whole list, no linuxulator.

**2. Build:**

```sh
git clone --recursive https://github.com/neoppix/intoxicated ~/intoxicated
cd ~/intoxicated
cargo build -p intoxicated-runtime --release
# produces target/release/intoxicated-run, a native FreeBSD binary
# (add --no-default-features to leave the visual effects out, or
# --features rt to add the experimental ray tracing)
```

**3. Install the launcher** (as root):

```sh
install -m 755 packaging/freebsd/intoxicated /usr/local/bin/intoxicated
```

It expects the checkout at `~/intoxicated`; set `INTOXICATED_REPO` if yours is
elsewhere.

**4. Run:**

```sh
intoxicated              # home page
intoxicated <placeId>    # straight into a game
intoxicated '<link>'     # a roblox:// link, roblox.com's Play link, or a
                         # https://www.roblox.com/games/<id> page link
intoxicated --update-apk # fetch the newest Roblox build, then exit
intoxicated --update     # update Intoxicated itself (pull, build, reinstall)
intoxicated --update --effects     # the same, adding the visual effects
intoxicated --update --no-effects  # the same, taking them out
intoxicated --update --rt          # the same, adding the experimental ray tracing
intoxicated --default-flags  # print the flags the client sets itself (JSON)
intoxicated --ssr-looks  # print the reflections' settings and looks (JSON)
intoxicated --shader-looks  # the same for every effect, and the modes
                            # ({"available": false} when built without them)
intoxicated --version    # print the client's version line
intoxicated --help       # print the launcher's usage
intoxicated --profile alt [<placeId> | '<link>']
                         # run another profile (see below)
INTOXICATED_PROFILE=alt intoxicated   # the same, from the environment
```

To update Intoxicated itself, run `intoxicated --update` (the installer, from
the checkout), or by hand pull and rebuild, then install the launcher again as
in step 3:

```sh
cd ~/intoxicated
git pull --recurse-submodules
cargo build --release    # the client and the APK fetcher; add
                         # --no-default-features for no visual effects
```

The git history was restarted on 2026-11-23, so a clone made before then
cannot pull. The installer notices, offers to move it aside and clones again;
by hand, do the same. The profile and the downloaded Roblox build live outside
the checkout and are kept.

**5. Optional: open roblox.com's Play button in Intoxicated** (the browser asks
for a handler for `roblox-player` links):

```sh
# as root
install -m 644 packaging/freebsd/intoxicated.desktop /usr/local/share/applications/
install -m 644 packaging/branding/intoxicated-logo.png /usr/local/share/pixmaps/intoxicated.png
update-desktop-database /usr/local/share/applications
# as your user
xdg-mime default intoxicated.desktop x-scheme-handler/roblox-player x-scheme-handler/roblox
```

A join link is turned into `roblox://experiences/start?placeId=<id>`, plus the
server or private-server code it names, and handed to the engine once the home
page is up. roblox.com's Play link carries a one-time sign-in ticket; it is
dropped, never passed to the engine and never printed, and the launcher runs
itself again with the rewritten link so the ticket does not stay in its
arguments. A link that names no place, is to another site, or carries a
malformed server id or code is refused (exit 2) and nothing of it reaches the
engine. The launcher sets `INTOXICATED_DEEPLINK_RETRY=1`, which re-publishes a
link nothing answered (once a second, at most 40 times) and stops at the first
`Game.launch`. For testing: `INTOXICATED_DEEPLINK_NO_PUBLISH=1` holds the link
without publishing it (the client stays on the home page),
`INTOXICATED_DEEPLINK_PROBE=1` lets devctl's `bussub` print payloads, and
`INTOXICATED_WEBVIEW_TEST=<url>` publishes one synthetic `openWindow`.

Intoxicated does **not** ship Roblox and never will. When no build is present the
launcher fetches the official Android x86-64 build (about 240 MB) from APKPure, a
third-party mirror, and **refuses anything not signed by Roblox's own
certificate**, so a tampered download is caught rather than trusted. It keeps it
in `~/.cache/intoxicated/apk` and fetches it again if that is ever cleared.

Your sign-in, settings and Roblox's own storage live in the profile,
`~/.local/share/intoxicated/profiles/default`. `intoxicated --profile <name>`
or `INTOXICATED_PROFILE=<name>` (1 to 32 letters, digits, `-` and `_`; the
switch wins over the variable, which wins over settings.conf) runs another
one, in `profiles/<name>`, with its own account, storage and `flags.json`;
one client runs per profile, so two profiles can play at the same time (see
[Several clients at once](#several-clients-at-once)). A second client on a
profile that is already open is refused with exit status 3, and the launcher
says which profile it was.

The launcher also reads `~/.config/intoxicated/settings.conf`
(`$XDG_CONFIG_HOME`): one `INTOXICATED_*=value` line per switch, exported
unless that variable is already set in the environment, which wins. The value
is taken literally and the file is never run as shell; other lines are
ignored.

As on Android, the engine is told this machine's IPv4 addresses when it asks
(`NetworkUtils.getPublicIPv4Addresseses`): every interface but loopback, so a
LAN address too. Nothing is sent to find them.

Roblox's assets are read straight out of the APK. The only one written to disk is
the CA bundle, in `~/.cache/intoxicated/assets`.

The engine looks for Android system fonts under `/system/fonts`; the client
answers from `~/.cache/intoxicated/system`, a tree of links to the fonts in
`/usr/local/share/fonts`, `~/.local/share/fonts` and `~/.fonts`.

The launcher also keeps two flags in your profile's `flags.json`, adding each
only if it is missing so a value you set by hand wins:

- `DFIntTaskSchedulerTargetFps=9999` lifts Roblox's 60 fps Android cap.
  `INTOXICATED_FPS=<n>` picks another value, `INTOXICATED_FPS=off` skips it.
- `FIntRenderForceVideoMemorySize` raises the Android build's fixed 64 MB video
  memory budget to 1.5 GiB. `INTOXICATED_VIDEO_MEMORY=<bytes>` or `=off`.

The client itself turns off memory savers that Roblox's servers hand the
Android build for phones: `FFlagRenderShadowResolutionScaling`, which shrinks
the shadows of lights to as little as 114 pixels and keeps rebuilding them,
and the Vulkan allocator settings that pool only small allocations and free
every empty page at once (`FFlagVulkanAllocatorPoolSmallOnly`,
`FIntVulkanAllocatorKeepEmptyPages`, `FFlagVulkanAllocatorEmptyPageBudget`).
The allocator ones cost a few hundred MB more VRAM. A value for any of these
keys in `flags.json` overrides the client's.

`target/release/intoxicated-run --help` lists the client's options and
environment switches. Among them, `INTOXICATED_RESOLUTION=<w>x<h>` sets the
render size (1280x720 by default; a fullscreen monitor overrides it) and
`INTOXICATED_FILES_DIR=<dir>` moves Roblox's data out of the profile. The
client no longer runs `nvidia-settings` to force NVIDIA's PowerMizer to maximum
performance (the setting did not hold on FreeBSD's driver and cost about 0.1 s
of every start), so `INTOXICATED_GPU_MAXPERF` is gone, and the early client
settings are always sent, so `INTOXICATED_EARLY_SETTINGS` is no longer needed.

## Visual effects

Intoxicated can draw its own effects into the game's picture, before the HUD is
drawn, so menus, chat and the leaderboard are never blurred, reflected or tinted
(a ReShade-style overlay cannot do that). All of them are off by default and
switched on one by one, or all at once with a mode:

| Effect | Switch | What it does |
|---|---|---|
| Reflections | `INTOXICATED_SSR=1` | wet, glossy floors reflecting what is on screen |
| Depth of field | `INTOXICATED_DOF=1` | a camera-lens blur of the far background, or of everything but your character for clips |
| Bloom | `INTOXICATED_BLOOM=1` | a glow around bright lights and neon |
| Lens | `INTOXICATED_LENS=1` | chromatic aberration, a vignette, a halo round bright areas |
| Sharpen | `INTOXICATED_SHARPEN=1` | contrast adaptive sharpening |
| Modes | `INTOXICATED_SHADERS_MODE=competitive` or `cinematic` | every effect at once, from a playable or a clip look |

The settings app's Shaders page sets all of this. The effects are optional:
the installer asks whether to build them in (no by default), and a client
built without them ignores every variable here and says so once in its log.
`intoxicated --update --effects` adds them later and
`intoxicated --update --no-effects` takes them out again (see
[Get it running](#get-it-running)). Each effect is described in detail below.

Screen-space reflections are an experimental extra, off by default:
`INTOXICATED_SSR=1` (in the environment or in `settings.conf`) makes surfaces
that face up look wet and reflect what is on screen. The client draws them
into the 3D scene before the HUD, with compute passes of its own recorded into
the engine's command buffer through Vulkan; nothing of the game itself is read
or changed. The default look, tuned on Timebomb Duels, is meant for
competitive play: faint, slightly blurred reflections on floors only, never a
sharp mirror. `INTOXICATED_SSR_LOOK` starts from another named look
(`competitive`, `wet_road`, `clean_mirror`, `puddles` or `glossy`), and every
look is a switch of its own (`INTOXICATED_SSR_INTENSITY`, `_ROUGHNESS`,
`_FRESNEL`, `_UP_ONLY`, `_UP_THRESHOLD`, `_MAX_DISTANCE`, `_EDGE_FADE`,
`_DARKEN`, `_TINT`, `_KEEP_COLOUR` and more; `--help` lists them with their
defaults), which wins over the look. The Shaders page of
[Intoxicated Settings](#intoxicated-settings) sets all of them, and
`intoxicated --ssr-looks` prints every one with its range and the value each
look gives it. On an RTX 4070 Ti they took about 0.55 ms of GPU time per frame at
3362x1349 and 0.6 ms at 3440x1440. Like
any screen-space effect they can only show what is on screen: anything off
screen or hidden behind something else is missing from the reflection, which
fades out towards the screen edges.

Depth of field and bloom are two more experimental extras, each off by default
and switched on its own: `INTOXICATED_DOF=1` and `INTOXICATED_BLOOM=1`. They
are drawn at the same point as the reflections, after them and in that order,
so the HUD is drawn over all three and is never blurred, reflected or made to
glow. Neither shows anything the engine did not draw: the depth of field only
blurs the picture, and the bloom only adds a glow around what is already
bright, so fog and darkness stay as they are.

- Depth of field keeps the focus sharp and blurs what is behind it (and, with
  `INTOXICATED_DOF_NEAR_BLUR=1`, what is in front), more the further away,
  like a camera lens, with a round bokeh; the sky counts as far. A blurred
  background does not bleed over a sharp edge in front of it. The default
  look, `playable`, focuses past your character on what lies behind it
  (`INTOXICATED_DOF_FOCUS_MODE=2`) and keeps 1.5 times that distance sharp
  behind it (`_FAR_RANGE`), with a light blur (`_STRENGTH=5`, a radius in
  pixels at 1080p), so in third person the play area stays sharp and only
  the far background and the sky soften. For clips, `cinematic` and
  `portrait` focus on your character at the screen centre (mode 1) and blur
  everything behind it strongly, `portrait` in front too
  (`INTOXICATED_DOF_LOOK`). Mode 0 focuses at `_FOCUS_DISTANCE` studs. The
  focus eases over `_FOCUS_TIME` seconds so it does not pump; `_SAMPLES` is
  the quality.
- Bloom makes bright lights, neon and bright sky glow:
  `INTOXICATED_BLOOM_THRESHOLD` is the brightness it starts at, from 0 to 1
  (the engine's picture is not HDR), with a soft `_KNEE`; `_INTENSITY`,
  `_RADIUS` (tight to wide) and `_TINT` shape the glow. Looks: `soft` (the
  default), `neon`, `dreamy` (`INTOXICATED_BLOOM_LOOK`).

devctl takes `dof` and `bloom` with the same words as `ssr` (`on`, `off`,
`get`, `set <name> <value>`, `look <name>`, `reset`, `stats`), and the Shaders
page sets all of it. On an RTX 4070 Ti at 1667x1349 on Timebomb Duels the depth
of field took about 0.13 to 0.2 ms of GPU time per frame and the bloom about
0.1 ms; both together, with the shared depth copy and copy back, about 0.3 ms,
and the frame times (p50 4.2 ms, about 240 fps) did not change measurably.
Limits: the depth of field works from the scene's depth, which glass, water and
other transparent surfaces usually do not write, so they are blurred or kept
sharp by what is behind them (INFERRED); the autofocus looks only around the
screen centre, so in shift-lock (character off centre) it focuses on what the
crosshair is on; mode 2 can settle on the floor just behind the character when
little else is in view (INFERRED); and the studs of the fixed focus assume a
near plane of 0.1 studs, which was not measured.

Lens effects and sharpening are two more, each off by default and drawn
after the bloom, sharpening last, so the HUD is never split into colours or
sharpened:

- `INTOXICATED_LENS=1`: chromatic aberration, red and blue pulled apart
  towards the screen edges and not at all in the middle
  (`_ABERRATION`, pixels at 1080p in the corners, default 1.5), a vignette
  (`_VIGNETTE`, 0.15) and a rainbow fringe just outside the brightest areas,
  taken from the bloom's chain (`_HALO`, 0.3; the lens runs the chain
  itself when the bloom is off). Looks: `subtle` (the default), `prism`,
  `film`.
- `INTOXICATED_SHARPEN=1`: contrast adaptive sharpening, written for
  Intoxicated from the idea of AMD's FidelityFX CAS: fine texture detail is
  raised most where the neighbourhood has room for it, edges already at full
  contrast barely, and no pixel changes by more than `_LIMIT` (0.1).
  `_STRENGTH` is 0.5; `_DEPTH_AWARE=1` (the default) keeps it from reaching
  across depth edges, so outlines get no halo. Looks: `light` (the default),
  `crisp`, `crunchy`.

devctl takes `lens` and `sharpen` with the same words. At 3362x1349 on an
RTX 4070 Ti on Timebomb Duels the sharpen took about 0.14 ms of GPU time per
frame alone and about 0.2 ms after other effects (it copies the scene once
first), the lens about 0.11 ms with the bloom on and about 0.15 ms when it
runs the bloom chain itself; frame times did not change measurably (p50 4.2
ms). Neither shows anything that is not drawn: the aberration moves drawn
colours by a few pixels, the vignette only darkens and the fringe only adds
light around what is already bright. Limits: the fringe shows mostly as a warm
yellow glow round white surfaces rather than a clear rainbow, faint at the
default and clear at prism's 1.0; what depth-aware sharpening changes along
an outline could not be told apart in game, as the character moved between
the shots; and like the other effects both work on the engine's 10-bit
picture, not HDR.

`INTOXICATED_SHADERS_MODE` switches all five at once and starts each from a
look: `off`; `competitive`, pretty but playable (the reflections' competitive
look, the depth of field's playable focus past your character, the soft
bloom, light sharpening, no lens effects); or `cinematic`, the clip look
(clean_mirror reflections, the depth of field focused on your character, the
neon bloom, the prism lens and crunchy sharpening). An effect's own switch or
look variable wins over the mode, and its knobs change the look as always;
devctl `shaders mode <name>` does the same while the game runs and `shaders`
reports each effect's switch and look. At 3362x1349 on the RTX 4070 Ti the
competitive set took about 1.3 ms of GPU time per frame and the cinematic
set about 1.5 ms, everything included (at 1667x1349 about 0.6 and 0.7
ms); frame times did not change measurably (p50 4.2 ms either way, as with
all off).

To compare with and without, or to get the frame time back for a round,
`INTOXICATED_EFFECTS_HOTKEY=1` (the Shaders page's "F9 turns all effects on
and off in game") makes F9 a hotkey: pressed in game it turns every effect
off, and pressed again puts back exactly the switches and looks they had,
live, without a restart. While they are off nothing of theirs is recorded
into the frame, so they cost no GPU time. It is off by default. While it is
on, Roblox no longer sees F9, so F9 does not open Roblox's developer console;
F9 still goes to Roblox while a text box such as the chat has focus, or with
Shift, Ctrl or Alt held. A key name instead of `1` (`F1` to `F10` or `F12`)
takes that key instead. Each press is one line in the log, and devctl
`effects toggle|on|off|get` does the same without a key. A client built
without the effects ignores the setting, says so once, and leaves F9 to
Roblox.

Ray-traced reflections are an experimental, opt-in alternative to the
screen-space ones; see [Experimental: ray-traced reflections](#experimental-ray-traced-reflections).

## Macros

A macro binds a key to a recorded sequence of input. When the key is pressed
with the Roblox window focused, the client plays the sequence back through its
own input path, the same path devctl's `move`, `click`, `key` and `text` use;
nothing is sent to the X server, so a macro never lands on the desktop and only
ever runs in game. The steps are taps and key holds, pointer moves, clicks,
scrolls, typed text and waits. A macro can also keep its trigger key from the
game, and repeat its steps while the key is held.

Macros live in `<profile>/macros.json`, read once at launch. The
[Macros page](#intoxicated-settings) of Intoxicated Settings writes the file;
it can also be edited by hand, as a `{"intoxicated-macros": 1, "macros": [...]}`
object whose macros carry a `name`, a `trigger` (a `key` evdev code and
optional `ctrl`/`shift`/`alt`), optional `enabled`, `swallow` and `repeat`, and
a `steps` array. A macro that cannot be read is skipped and the rest kept.
Macros are off by default: only `INTOXICATED_MACROS=1` (the page's "Run
macros" switch) turns them on. The in-game trigger and playback are not covered by the automated
tests (they need a running client); the file's parsing, the step model and the
settings app's editor are.

When the disk holding the profile's Roblox data runs short, the client says so:
below 2 GiB free as it hands Roblox its window, below 512 MiB again when the
run ends. The engine itself never reports a full disk.

A new Roblox build can import symbols Intoxicated does not answer yet; if
`intoxicated` reports `cannot locate symbol`, that is what happened. The raw
syscall sites the anticheat fix needs are found automatically for every build.

## Experimental: ray-traced reflections

**Experimental. Off by default, opt in at your own risk.** It is only in a
client built with it (the installer's ray tracing question, `intoxicated
--update --rt`, or `cargo build --release --features rt`). Then
`INTOXICATED_RT=1`
(in the environment or settings.conf, or the switch under "Ray-traced
reflections (experimental)" on the Shaders page of Intoxicated Settings, which
asks for confirmation) replaces the screen-space reflections with ray-traced
ones; remove the line, or turn the switch off, to go back. No look or preset
turns it on. The launcher and the client both print a warning when it is on,
and devctl `rt stats` repeats it. The effects hotkey (F9, when on) turns it
off and back on with the effects.

What it does: each frame the client captures the opaque world geometry the
engine draws in its 3D scene (transform feedback, enabled through Vulkan: the
engine's lit vertex shaders get their position, colour, camera vector and
world position marked for capture and compute nothing differently), measures
the camera's real projection from it, keeps the static parts in a cache in
world space so they stay reflectable after they leave the view, builds a
bounding volume hierarchy on the GPU and traces one ray per 2 by 2 pixels
against it. A reflection can so show what is off screen or behind the camera,
such as the ceiling in a floor or players off screen in a wall. The screen-space reflections' look and fine tuning still apply.

What to know before turning it on:

- While on it replaces the screen-space reflections entirely.
- A hit takes its own colour, as the engine's shader makes it: the part's
  colour times its texture where the shader samples the texture at the
  coordinates it is given (clothing and faces), else the part's flat colour;
  lit as the engine lights it, with its sun (a ray to the sun for the
  shadow), its ambient and its exposure, read from the engine's per-frame
  uniform block, which the trace binds as the engine does.
  `INTOXICATED_RT_SHADE=own` leaves the light out (the colour at the
  brightness the screen shows such colours at on average), `mix` blends in
  a quarter of the screen's colour where the screen shows the hit, `screen`
  takes the screen's colour there; devctl `rt shade lit|own|mix|screen`.
- Interface drawn in the world, such as leaderboards, name tags and trophy
  icons, does not reflect, nor do particles or see-through parts (of what
  the game draws blended, only characters and Neon parts are traced, where
  their alpha is at least 0.95, and 0.05 for Neon).
- Reflections can flicker while the camera moves.
- Objects the game has never drawn cannot appear; a static part that moved
  while out of view stays in the reflections at its old place until the view
  shows that place again, and players only come from the current frame.
- It takes up to an eighth of the video memory, at most about 1.6 GB
  (room for two million triangles on a 12 GB card; `INTOXICATED_RT_MEMORY`
  sets the budget in MiB, `INTOXICATED_RT_TRIANGLES` the triangles), and
  about 2 to 3 ms more GPU time a frame, 3 to 6 ms with the indirect light. Cached parts are traced within
  512 studs of the camera (`INTOXICATED_RT_REACH`). Measured in Timebomb Duels at 1667x1349 on an RTX 4070 Ti: 0.2 to
  0.3 ms for the hierarchy and 0.5 to 0.7 ms for the rays (all of the
  reflections' work 0.9 to 1.3 ms, against 0.6 ms for the screen-space
  ones); frame times did not change at the engine's 240 fps. In Jailbreak
  (the city, its menu's camera flight) with terrain and Neon parts and the
  indirect light on, at 1667x1349: 120,000 to 260,000 triangles, 1.1 to 2.0
  ms for the hierarchy and 1.2 to 2.4 ms for the trace at 4 by 4 blocks (before the blocks were held at 2 by 2); the
  capture hooks cost the render thread about 0.5 ms a frame (0.6 us a draw).
- Large scenes cost more: Blox Fruits captured 830,000 triangles a frame
  and the hierarchy's build took 3.1 to 3.5 ms; Doors' many draws cost the
  render thread 1.0 to 1.2 ms a frame in the capture hooks (measured at
  1667x1349 in their menus and lobbies). Under the time budget the indirect
  light's blocks then grow (`rt stats` `gi_blocks`).
- It runs as compute on the shader cores because NVIDIA's FreeBSD driver
  has no hardware ray tracing: on the RTX 4070 Ti, drivers 580.142 and
  620.70.06 list neither `VK_KHR_acceleration_structure` nor
  `VK_KHR_ray_query` nor `VK_KHR_ray_tracing_pipeline` (vulkaninfo lists them
  for llvmpipe only), and the package ships no `libnvidia-rtcore`.
- It may crash or misbehave on other GPUs and drivers: it was only tested on
  an RTX 4070 Ti with NVIDIA 620.70.06, in Timebomb Duels.

Round surfaces reflect smoothly: each reflecting pixel's own triangle is found
with a ray from the camera and its normal is taken from the vertex normals the
engine's shader gave its corners; the screen-space reflections, which have no
captured geometry, even out the depth's normals over a ring of neighbours on
the same surface instead, which also applies to the ray-traced ones where no
triangle is found. Near the camera on a curved surface each pixel traces its
own ray. devctl `rt view normal` and `rt view source` show the normals and
where each came from (green the vertex normals, yellow evened out, red the
depth's) in place of the reflections; `rt view off` ends it. Rays that meet
nothing take the sky where the screen shows it in their direction now (through
a window or an opening), else from an environment the client builds from the
sky the screen has shown, kept until a teleport and blurred so its edges fade;
a direction the screen never showed sky in reflects nothing, since a ceiling
the game never drew may be there (`INTOXICATED_RT_ENV=0` or devctl `rt env off`
turns it off). Only parts that stayed in place for a second are kept in the
cache, so what a moving player wears leaves no trail.

Neon parts give off light: the client recognises the engine's Neon shader
(its colour `4 * pow(colour * 1.35, 4)`, unlit) and captures those parts even
where the engine draws them blending, so they show in reflections at their
own brightness (above white, so even a faint reflection of them glows), in
view or not. In the indirect light below, each 16 by 16 pixel tile lists the
eight Neon triangles that light it most, with whether they are in sight, and
their light is taken exactly (each triangle's form factor at half its engine
brightness, no random samples), so the glow beside a Neon panel is strong and
steady. A tile keeps the lights it chose the frame before (found through the
last frame's camera and favoured by half again) and weighs them against up to
512 more within 128 studs, so its choice holds from frame to frame; the sight
rays are cast one invocation per tile and light, new choices at once and kept
ones in turn every fourth frame. Reflected hits the screen does not show take
Neon light from eight random Neon triangles without a sight ray, so a Neon
part behind a wall can light them there. With the bloom on, what the ray-traced reflections and
the indirect light have above white (reflected Neon, its glow on a wall) is
added to what the bloom blooms (`INTOXICATED_RT_GLOW=0` or devctl `rt glow
off` leaves it out; INFERRED: the pass runs, but in the Timebomb Duels lobby
screenshots with it on and off did not differ, and no reflection bright
enough was in view to show it).

What moves is followed from frame to frame: each captured draw is matched to
the same draw the frame before (the same pipeline, index count and
instances, its same occurrence in the frame, as many triangles) and each of
its triangles to the same triangle then, kept where it has the same size and
moved less than 16 studs; a cached part drawn in the same place the frame
before has not moved. The triangle under a pixel (found with the ray from the
camera, within 48 studs) so gives its move, and the indirect light and the
reflections find their history where the point was the frame before rather
than where the camera alone puts it, and keep a short history while it moves
(4 frames for the light, 3 for the reflections). `rt stats` counts the
captured triangles whose move is known and that moved (`motion_known`,
`motion_moving`; measured: in Jailbreak's menu flight nearly every captured
triangle matched, in the Timebomb Duels lobby the walking players moved).
Farther than 48 studs, and for what was not matched, only the camera's move
is taken.
Terrain (grass, rock, sand) is captured too, without its swaying decoration
grass and without water: its shaders pass on no colour, so it is grey until
the screen has shown it, then in the colour the screen showed it in; terrain
reflects nothing itself (it is rough), and pixels of things the capture
leaves out (the decoration grass) get no indirect light, which would change
every frame. The ray from the camera that finds a pixel's triangle only
looks around the pixel's depth, so what is nearer and not shown (the
character's own head in first person) does not hide it. devctl `rt view category` colours what each reflection ray hit
(yellow Neon, green terrain, white a part that stays in place, grey one that
moves, dark blue nothing), and `rt kind neon|terrain on|off` leaves a kind
out, for bisecting.

Diffuse indirect light, more experimental still and off unless
`INTOXICATED_RT_GI=1` at start (devctl `rt gi on|off` then), a hybrid of
screen space and the traced scene, per block of pixels every other frame (in
a checkerboard). First two slices around the view direction are marched
across the screen, eight samples a side out to about 12 studs, with a
visibility bitmask (after Therrien, Levesque and Gilet, 2023): each sample is
taken as a slab about 0.75 studs deep behind what depth shows, the 32 sectors
of the slice it hides are marked, and its screen colour lights the sectors it
is first to hide, so nearby walls, floors and lit panels light a pixel with
their textures and the engine's own lamps while what is behind them stays
hidden. What no sample hid (the ray left the screen or passed everything)
continues as one traced ray into an open sector: a hit the screen shows takes
its light from the screen, any other hit is lit as the engine lights it, and
open sky gives the engine's own ambient from that side (the sky's colour on
the screen, taken as light, washed open places out). The Neon light is added
as above, at its full engine brightness (`INTOXICATED_RT_NEON_GAIN`, 1 unless
set, 0 to 4; devctl `rt neon-gain <x>`). The bounces count
`INTOXICATED_RT_GI_GAIN` times over (1.6 unless set, 0 to 4; devctl `rt
gi-gain <x>`). The point a block traces from moves over its block from frame
to frame. The result is gathered over up to 32
frames at the same point in the world (sooner forgotten when the light there
changes), smoothed by three a-trous filter passes that stop at edges in depth
and normal and between light and dark (guided by the noise measured at each
point, as SVGF does, but never narrower than a quarter of the light, so a
long history does not keep small steps between blocks), spread back over
the pixels by a joint bilateral upsample (a smooth kernel over the 4 by 4
blocks around each pixel, weighted by how near each block's point lies to
the pixel's plane and how alike their normals and colours are), kept over up
to 16 frames per pixel (held within what the blocks around show now), and
put in place of the engine's own ambient light (what it adds
rolls off softly towards eight times the ambient; a pixel gets at most 30%
darker), so corners darken and coloured walls tint what faces them. It is
computed before the reflections, so they show it. It takes about 160 MB more
video memory when on. Measured at 1667x1349: in the Timebomb Duels lobby, 2
by 2 blocks, 0.2 ms for the Neon tiles, 1.0 ms for the indirect light and 0.3
ms for its filter; with the camera still, two screenshots a second apart
differ by 0.2 to 0.5 of 255 on average.

A soft look (`INTOXICATED_RT_SOFT=1`, devctl `rt soft on|off`) traces on 4
by 4 blocks and blurs the reflections widely, stretched down the screen as a
glossy floor streaks them (up to 6 to 1, more the further the reflected thing
and the more grazing the view), stopping only where the receiving surface
changes, so it never bleeds onto walls or players and shows no steps.

The reflections trace one ray per block of 2 by 2 pixels at any size
(`INTOXICATED_RT_BLOCK=1` for one per pixel), from a different pixel of the
block each frame; each pixel keeps a history, found where the reflected thing
was a frame ago (its mirror image, by the hit distance), held to what the
pixels around it show now and kept over up to 12 frames while the camera is
still and 3 while it turns, so reflected edges come out smooth rather than in
block steps. The indirect light's blocks follow the output's size (2 by 2
up to about 2.4 million pixels, 3 by 3 up to about 5.4 million, as
3440x1440, else 4 by 4, held near a step so a resize does not swing them),
one pixel wider per step, up to two, while the trace takes more than the 4.5
ms budget leaves after the hierarchy's build for three looks in a row (two
seconds each at 60 fps), and back after three looks well under it, never
coarser than 3 by 3 up to 5.4 million pixels (4 by 4 above); their
history is carried across a change, not cleared (`rt stats`: `blocks`,
`gi_blocks`, and the trace's parts `lights_ms`, `gi_ms`, `gi_filter_ms`,
`reflections_ms`, of which `reflection_rays_ms` and `upsample_ms`).
Reflection rays end after 1024 studs (then the sky) and rays towards the sun
after 512. Measured fullscreen at 3440x1440 in Jailbreak's menu flight: build
1.3 to 1.6 ms, trace 3.4 to 5.1 ms (Neon tiles 0.4 to 1.3, indirect light 0.7
to 1.6 at 5 by 5 under the budget, reflections 2.0 to 2.5, of which their rays
1.1 to 1.6). Only at 2 by 2 with the indirect light not coarsened by
the budget do near curved pixels trace their own rays. `rt stats` also reports the render thread's time in the
client's capture hooks (`cpu_us_frame`, `cpu_ns_draw`).

devctl `rt on` and `rt off` switch it while the game runs (with
`INTOXICATED_RT=1` at start), `rt stats` reports the triangles, the textures,
the measured camera, the cache and the GPU time, and `rt dump-cache <path>`
writes the cached triangles out. A cached part is dropped only once the view
has shown its place empty, with nothing drawn in front of it, for 8 frames in
a row while the frame did not draw it.

## Intoxicated Settings

An optional desktop app for what is otherwise set by hand. FastFlags: presets
(frame rate cap, video memory budget, light shadows, GPU memory pooling, 4x
MSAA), every flag in the profile's `flags.json` with inline editing, import of
a JSON file (a Bloxstrap export or a `ClientAppSettings.json`) with a preview,
export and reset. Client options: window, input, graphics, audio and session.
Shaders: at the top the modes Off, Competitive and Cinematic, with Custom
lit when the effects were changed by hand (picking a mode sets every
effect's switch and look and clears the fine tuning, after asking); then the
screen-space reflections, the depth of field, the bloom, the lens and the
sharpen, one group each with a switch, the named looks (Competitive, Wet
road, Clean mirror, Puddles and Glossy for the reflections; Playable,
Cinematic and Portrait for the depth of field; Soft, Neon and Dreamy for the
bloom; Subtle, Prism and Film for the lens; Light, Crisp and Crunchy for the
sharpen) and a fine-tune row for each of their settings (the depth of
field's focus as Fixed distance, Character or Past the character); picking a
look clears that effect's fine tuning, after asking, and each row has its
own reset. Below them, marked experimental, the switch for the ray-traced
reflections and light, with their warnings and a confirmation; a client
built without the ray tracing shows instead how to add it (`intoxicated
--update --rt`). Like the client options they apply from the next launch.
With a client built without the visual effects the Shaders page only says
how to add them (`intoxicated --update --effects`). Macros:
the profile's in-game macros, a list on the left and the selected one's trigger
key, Ctrl/Shift/Alt, and steps on the right, with a master switch for the lot;
see [Macros](#macros) below. Profiles: create, delete and pick the default one. And Play, a place id or
link field, Run several and Running (see
[Several clients at once](#several-clients-at-once)), and the two update
commands. It is a Qt 6 Widgets program in
`settings/` that needs `qt6-base` and nothing else.

The installer asks whether to install it (the answer defaults to yes) while it
checks the packages, so Qt is installed with the rest; it builds the app as
your user and installs it with the launcher. Once it is installed, every later
run updates it without asking, and a failed build of the app does not stop the
rest of the install. By hand:

```sh
pkg install qt6-base                     # as root
cmake -S settings -B settings/build -DCMAKE_BUILD_TYPE=Release
cmake --build settings/build -j "$(sysctl -n hw.ncpu)"
# as root
install -m 755 settings/build/intoxicated-settings /usr/local/bin/
install -m 644 packaging/freebsd/intoxicated-settings.desktop /usr/local/share/applications/
```

It writes two kinds of file, and both can still be edited by hand:

- `~/.config/intoxicated/settings.conf`: the client options, read by the
  launcher as above. A control at its default removes its line; lines the app
  does not manage, comments included, are kept.
- `~/.local/share/intoxicated/profiles/<profile>/flags.json`: the profile's
  flags, one JSON object. A file that is not a JSON object is never written
  over; the app offers to keep it as `flags.json.bad-<time>` and start empty.

Both are written to a temporary file and renamed into place. The presets show
the client's own flags, which the app reads from `intoxicated --default-flags`,
and the Shaders page shows the client's own effects, looks and values, read
from `intoxicated --shader-looks`, so the app keeps no copy of either.
A profile shows as in use while a client holds it, and is not deleted then.
"Update Intoxicated" runs `intoxicated --update` with no terminal, so the
installer asks for the password through the app's own dialog (`sudo -A` with
`SUDO_ASKPASS`). `ctest --test-dir settings/build` runs the app's self-test
of its file handling, against scratch directories.

**Macros: use at your own risk.** The Macros page plays back recorded input.
Roblox's Community Standards forbid gaining an unfair advantage and using
automation for disruptive tasks, and many games ban macros in their own rules
and moderate them themselves. Whether a macro is allowed depends on the game
you use it in; check its rules first. A macro that breaks a game's rules can
get you kicked or banned from that game, and that is on you, not on
Intoxicated.

## Several clients at once

**Roblox does not officially support running several clients at once, and a
Byfron developer has said that running multiple clients may be treated as
malicious. Use alt accounts, and do it at your own risk.** Intoxicated does
nothing to get around anything of Roblox's: it has no single-instance check
to defeat. Each client is simply its own process on its own profile.

Every profile is its own Roblox account and storage, and one client runs per
profile, so starting several means one launch per profile:

```sh
intoxicated &                         # default
intoxicated --profile alt &           # a second account, in its own window
intoxicated --profile farm_2 189707 & # a third, straight into a game
```

A profile that is not signed in opens at the Roblox login screen in its own
window. The window title names the profile when it is not `default`
(`Intoxicated <version> (<commit>) [alt]`); `WM_CLASS` is `intoxicated`,
`Intoxicated` for all of them, so window rules still match every one.

In Intoxicated Settings, the Play page has **Run several**: a switch per
profile, with whether it is signed in and whether it is running now, and
**Play selected**, which starts one launcher per checked profile (with
`INTOXICATED_PROFILE` set), a second and a half apart. The first time in a
run of the app that this would leave more than one client running, it shows
the warning above with Continue and Cancel. **Running** lists the clients
running now, read from the profiles' locks every two seconds, each with a
**Stop** button. While a client holds its profile it writes its process id to
`<profile>/client.pid` and removes it when it exits; Stop sends that process
SIGTERM, which closes it the way Ctrl-C does, and only while that process
holds the profile's lock open, so a file left by a crash or a process id
used again since is never signalled. Nothing is ever stopped by name.

What clients on different profiles share, and how it is kept safe:

- The Roblox build in `~/.cache/intoxicated/apk` is only read. When there is
  none yet, the first launch downloads it and the others wait for that
  download instead of failing.
- `~/.cache/intoxicated/clientsettings.json` and
  `~/.cache/intoxicated/syscall-sites/` are written through a temporary file
  of each writer's own and a rename, so two clients refreshing them together
  leave one whole file. The asset folder (`~/.cache/intoxicated/assets`) is
  written the same way, and the font links in `~/.cache/intoxicated/system`
  are replaced by a rename, so a running client never finds one missing
  while the next client starts.
- settings.conf is only read by the launcher, and every client reads the
  same options from it. The devctl socket, macros, logs, the run directory
  and Roblox's own storage are all in the profile. A devctl socket path
  shared through `INTOXICATED_DEV_CONTROL_SOCKET` is not taken from a client
  still listening on it; the second client goes without devctl and says so.
- Each client owns the X11 `CLIPBOARD` only while its last copy is the
  newest, as any X program does.
- Sound: every client opens `/dev/dsp` itself, and sound(4)'s virtual
  channels mix them. With `dev.pcm.N.play.vchans=1`, the default, three
  opens of the same device at once all succeed; a device with vchans off
  needs them turned on for a second client to play.
- Intoxicated Settings sees whether a profile is in use by taking its lock
  for an instant; a client starting just then waits up to 0.3 s for it
  rather than refusing.

## Status: playable

Intoxicated is used for everyday play: full matches, long sessions and fast
competitive games run start to finish. Reason-304 is solved, so the client
stays in the game. Sign-in, loading a game,
moving around, mouse and keyboard (including shift-lock, Ctrl+A/C/X/V and real
clipboard), uncapped framerate, held-key input, and audio all work. Keys reach
the game by their position on any keyboard layout, with any modifiers held
(sprint plus a number key, Shift+Tab, the keypad), and chat takes accented
letters and dead keys. Meshes load at
full detail, and on NVIDIA, whose Vulkan driver has no ETC2 textures, Intoxicated
decodes them itself so the textures the engine would otherwise drop are drawn.

The ETC2 decode runs across several cores (a 50 MB burst while a map loads
decodes in about 60 ms), and the memory it decodes into is reused rather than
allocated again for every frame, which on NVIDIA is a slow kernel call. In
Frontlines, the seconds after deploying used to bring 15 to 18 frames of 100
to 800 ms and once a 3.3 s freeze; now 3 to 6 frames of up to about 150 ms,
and 2.5 minutes in game afterwards had 9 frames over 40 ms, none over 250 ms.
`intoxicated <placeId>` joins the place directly.
Game audio goes straight to OSS with about 10 ms of device latency, so it works
without PulseAudio and sounds land in time with what is on screen.
Keys reach the game by their physical position, so Shift plus a number switches
weapons while sprinting and non-US layouts work, and mouse motion always
arrives before the click that follows it, so a flick and a shot land in order.

Known problems:

- Sun shadows use the 1024-pixel shadow maps the Android build picks and look
  blocky in the distance; no setting that raises them has been found yet.
- Joining a server still stalls for about 2 s in all while the map loads,
  most of it the engine's own work.
- About every 45 s the engine's own periodic work holds one frame for about
  100 to 130 ms.
- Now and then, once in a match, a frame freezes for up to about 3 s shortly
  after deploying or at the first death; the cause is not found yet.
- Voice chat is unimplemented. Game controllers are not supported yet: the old
  support read Linux-only joystick paths and never found a device on FreeBSD,
  so it was removed; native FreeBSD (evdev) support is still to be written.

## License

Licensed **GPL-3.0-or-later**, see [`LICENSE`](LICENSE). The build links two
MIT-licensed projects, ChristopherHX and MCMrARM's
[mcpelauncher-linker](https://github.com/minecraft-linux/mcpelauncher-linker)
(the AOSP bionic linker port) and ChristopherHX's
[libjnivm](https://github.com/ChristopherHX/libjnivm); their notices are in
[`NOTICE`](NOTICE) and [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). Roblox and the Roblox engine
are the property of Roblox Corporation and are not redistributed here.
