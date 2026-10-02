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
  <em>A fork of <a href="https://github.com/luohoa97/cordial">cordial</a> by
  <a href="https://github.com/luohoa97">luohoa97</a> and the Cordial contributors.
  Cordial does the hard part, a whole Roblox runtime for Linux. Intoxicated carries
  it onto FreeBSD. GPL-3.0, same as upstream.</em>
</p>

<p align="center">
  <a href="https://discord.gg/qJzU3Xfr9b">
    <img src="https://img.shields.io/badge/Discord-cordial%20community-5865F2?style=for-the-badge&logo=discord&logoColor=white"
         alt="Cordial / Intoxicated Discord">
  </a>
</p>

<p align="center">
  Bugs and feature requests for the FreeBSD port go on
  <a href="https://github.com/neoppix/intoxicated/issues">this repo's issues</a>,
  not in chat, so they don't get lost. The <a href="https://discord.gg/qJzU3Xfr9b">cordial
  Discord</a> is the shared community for both.
</p>

<p align="center"><em>A hobby project, not a commercial one. Please don't DMCA it.</em></p>

## What it is

Intoxicated loads Roblox's official **Android x86-64** engine directly on FreeBSD
through cordial's purpose-built runtime: the AOSP bionic linker, a bionic/libc
shim, a JNI VM in place of Android's, and a framework layer that answers the
client's calls.

**It runs native, all the way down.** The `cordial-run` binary is a native
FreeBSD ELF, not a linuxulator program. The graphics go through native FreeBSD
Vulkan or GLES2. The input goes through native X11. And `libroblox.so` itself, the
Linux/Android engine, is loaded and executed by cordial's **own** bionic linker as
native FreeBSD code, with FreeBSD-specific shims wherever the two ABIs disagree
(syscalls, `pthread`, `environ`, `/proc`, futexes). Nothing emulates Roblox and
nothing runs it under the Linux ABI. It talks to your GPU the way any native app
does.

The headline port work: Roblox's anticheat kicks for **reason 304** on FreeBSD,
because `libroblox.so` makes raw kernel-ABI syscalls the bionic setup can't route
cleanly. Intoxicated patches all 40 raw syscall sites in the engine to `ud2` and
catches the resulting `SIGILL`, routing each through `bionic_syscall`, so the
anticheat stops seeing the thing it was killing the client for. That fix is what
makes any of the rest matter.

## Get it running

This is a **build-from-source hobby port** on FreeBSD, tested on FreeBSD
14.4-RELEASE with an NVIDIA GPU (RTX 40-series), native X11, and native Vulkan.

**Zero `/compat/linux` at runtime.** The engine reads Linux `/proc` and `/sys`;
every one of those reads (`meminfo`, `cpuinfo`, the process list the anticheat
scans, `self/{fd,auxv,status,maps}`, and the rest) is answered from FreeBSD's own
`sysctl`/`elf_aux_info`, not from linprocfs. There is no linuxulator and no
Linux-compat filesystem mount to set up. Verified: a run makes **0** reads under
`/compat/linux`.

**1. Prerequisites** (as root):

```sh
# Rust, native X11, native Vulkan loader
pkg install rust libX11 libXi libXinerama vulkan-loader

# Native GPU driver (this is what provides the real Vulkan ICD)
pkg install nvidia-driver          # NVIDIA; use the Mesa drivers for AMD/Intel
sysrc kld_list+="nvidia-modeset" && kldload nvidia-modeset
```

You also need a running X11 session. That is the whole list, no linuxulator.

**2. Build:**

```sh
git clone https://github.com/neoppix/intoxicated
cd intoxicated
git checkout freebsd-port
cargo build -p cordial-runtime --release
# produces target/release/cordial-run, a native FreeBSD binary
```

**3. Get Roblox's Android build.** Intoxicated does **not** ship Roblox and never
will. On first run it fetches the official Android x86-64 build from APKPure (a
third-party mirror) and **refuses to install anything not signed by Roblox's own
certificate**, so a tampered mirror is caught rather than trusted. It is a few
hundred megabytes and waits for you to ask before downloading. You can also point
it at your own APK with `CORDIAL_ROBLOX_APK_URL`, or reuse the one
[Sober](https://sober.vinegarhq.org/) already downloaded.

**4. Run:**

```sh
./target/release/cordial-run \
  --lib-dir "$HOME/.cache/cordial-apk-new/lib/x86_64" \
  --apk     "$HOME/.cache/cordial-apk-new/candidate-0.apk" \
  --host-libc --game-activity --run 0
```

Install the `intoxicated` launcher script to your path for the short form
(`intoxicated`, or `intoxicated <placeId>` to join a game). It wraps that command
with sane defaults.

## Status: early, but it plays

Reason-304 is solved, so the client stays in the game. Sign-in, loading a game,
moving around, mouse and keyboard (including shift-lock, Ctrl+A/C/X/V and real
clipboard), full framerate, held-key input, and audio all work. Known rough edges:
in-game texture and mesh detail is lower than desktop Roblox (an Android-render-path
limit still being chased), voice chat is unimplemented, and controllers are
untested.

## Credit and license

Intoxicated is a fork of **[cordial](https://github.com/luohoa97/cordial)** by
**[luohoa97](https://github.com/luohoa97)** and the Cordial contributors. The
runtime, the architecture, and the years of reverse-engineering that make running
Roblox outside Android possible are theirs. Intoxicated is the FreeBSD-specific
layer on top.

Licensed **GPL-3.0-or-later**, the same as cordial. See [`LICENSE`](LICENSE) and
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). Roblox and the Roblox engine
are the property of Roblox Corporation and are not redistributed here.
