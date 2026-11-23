# Third-party notices

Intoxicated as a whole is GPL-3.0-or-later (see [`LICENSE`](LICENSE)). It
incorporates the components below, each under its own licence, and each of those
licences requires its notice to travel with the software, in source and in
binary form. This file exists so that obligation is met by anyone redistributing
Intoxicated.

MIT and Apache-2.0 are both compatible with GPL-3.0 in this direction: their
terms are satisfied while the combined work is offered under the GPL. Preserving
these notices is a condition of that, not a courtesy.

---

## mcpelauncher-linker: MIT

The host wrapper around the AOSP bionic linker. Submodule at
[`third_party/mcpelauncher-linker/`](third_party/mcpelauncher-linker), from
[`minecraft-linux/mcpelauncher-linker`](https://github.com/minecraft-linux/mcpelauncher-linker).

```
MIT License

Copyright (c) 2024 ChristopherHX and MCMrARM
```

Full text: `third_party/mcpelauncher-linker/LICENSE`.

---

## Android Open Source Project (bionic): Apache-2.0 and BSD

The bionic linker and headers carried inside `mcpelauncher-linker`. The NOTICE
file required by Apache-2.0 §4(d) is preserved at
`third_party/mcpelauncher-linker/core/NOTICE`.

```
Android Code
Copyright 2005-2008 The Android Open Source Project
```

Portions of bionic are under BSD licences from the original authors, whose
notices are preserved in the individual source files as required.

---

## libjnivm: MIT

The JNI virtual machine that stands in for Android's. Submodule at
[`third_party/libjnivm/`](third_party/libjnivm), from
[`ChristopherHX/libjnivm`](https://github.com/ChristopherHX/libjnivm).

```
MIT License

Copyright (c) 2019 ChristopherHX
```

Full text: `third_party/libjnivm/LICENSE`.

---

## Android Game Development Kit: Apache-2.0

Not vendored. AGDK's `GameActivity` source was **read** to get the
`onTouchEventNative` argument packing and the surface/lifecycle callback
contract right rather than guessing at them. No AGDK code is copied into this
repository; the reference is recorded here because it is the reason those
signatures are correct.

---

## What is *not* here

**Roblox.** Intoxicated contains no Roblox code, APK, asset or decompiled material,
and never will. It loads the official Android client that the user supplies from
their own installation. Roblox is a trademark of Roblox Corporation, which does
not endorse this project.
