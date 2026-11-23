# AGENTS.md

How work on Intoxicated is done. Read this before changing anything.

Intoxicated runs Roblox's Android x86-64 engine as a native FreeBSD process.
Its code is written here; what comes from elsewhere is under `third_party/`
with its own licence (see `NOTICE`).

## FreeBSD only

Everything here is native FreeBSD. Never use Linux, the linuxulator, anything
under `/compat/linux` or a Linux binary, not even for a quick check.

A coding agent may itself run under the linuxulator. Its shell then resolves
`sh`, coreutils, `sed`, `grep` and the like to Linux copies first. Run every
command through the native Python with a FreeBSD PATH instead:

    /usr/local/bin/python3.11 -c "import subprocess,os; subprocess.run(['/bin/sh','-c','CMD'], cwd='REPO', env={'HOME':os.environ['HOME'],'PATH':'/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin'})"

`uname -s` run that way prints `FreeBSD`.

## Verify by running

A change is finished when it has been run, not when it compiles.

1. `cargo build --release`, with no warnings that were not there before.
2. `cargo test --workspace --release`, with zero failures.
3. The load check. It links the engine and patches its raw syscall sites
   without opening a window, and must print `LOADED` and `neutralised 16/16`
   (the count belongs to the current Roblox build). Point `XDG_DATA_HOME` at a
   scratch directory so it leaves the real profile alone:

       XDG_DATA_HOME=/path/to/scratch target/release/intoxicated-run \
         --lib-dir ~/.cache/intoxicated/apk/lib/x86_64 \
         --apk ~/.cache/intoxicated/apk/candidate-0.apk --host-libc

4. Behaviour in game is checked in game, through the devctl socket: start the
   client with `INTOXICATED_DEV_CONTROL=1` and talk to `<profile>/devctl.sock`.

Anything not tested this way is labelled INFERRED, in the code comment and in
the commit message. Never report a result that was not observed.

Do not measure while something compiles. Frame times, CPU use and load times
taken next to a build are noise; wait for the build or stop it first.

## Input

Never synthesise input at the X server (XTest, xdotool or anything like them).
It lands on whatever has focus, which is the user's desktop. Drive the client
through devctl, which feeds its own input path and reads its own frames.

## One client per profile

`intoxicated-run` holds an flock on `<profile>/.lock` for as long as it runs and
refuses a second client on the same profile with exit status 3. Two clients on
one profile would corrupt its Roblox storage and session. For a parallel run,
use another `--profile <name>` or its own `XDG_DATA_HOME` on disk (`/tmp` is
tmpfs and costs RAM).

Profiles live in `$XDG_DATA_HOME/intoxicated/profiles`, by default
`~/.local/share/intoxicated/profiles`. The launcher keeps the Roblox build in
`~/.cache/intoxicated/apk`.

## How users run it

Users run the launcher, `packaging/freebsd/intoxicated`, installed as
`/usr/local/bin/intoxicated`. It fetches and verifies the APK, keeps the fps
and video memory flags in the profile and starts `intoxicated-run` with the
switches the client needs. A change that needs a new switch, path or flag
belongs in the launcher as well. Running `target/release/intoxicated-run`
directly, as in the load check (add `--game-activity --run <secs>` to bring
the engine up), is for debugging, not the way the client is meant to be
started.

## Matching code from elsewhere

Code is never copied in from another project. When behaviour has to match
someone else's code, it is written clean-room, by two different people or
agents:

- The spec writer reads the other code and the running client and writes down
  behaviour only: inputs, outputs, errors and observable effects. The spec
  contains no code and says nothing about how the other code is structured.
- The implementer works from the spec alone and never opens the other code or
  its history.

Every name in the tree is Intoxicated's: the `intoxicated-runtime`,
`intoxicated-linker-sys` and `intoxicated-fetch` crates, the `intoxicated-run`
binary and the `INTOXICATED_*` switches, and new names follow suit.

## Writing

- No emojis, no em dashes and no en dashes, in code, comments, docs and
  commit messages.
- No session links in commit messages or anywhere else in the tree.
- A commit message says what changed and what was measured to check it.
- LICENSE, NOTICE and THIRD-PARTY-NOTICES.md carry the licence terms of the
  code under `third_party/`; keep them in step with what is linked in.
- Check README.md against the change before every push: paths, commands,
  switches and the list of known problems must match what the code does now.
