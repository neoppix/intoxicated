//! The command line: the options, the usage text, the parse errors,
//! `--check-join-url`, `--print-default-flags`, `--print-ssr-looks` and
//! `--print-shader-looks`.
//!
//! Arguments are read left to right. A flag that takes a value consumes the
//! next argument whatever it looks like, a repeated flag replaces the earlier
//! value, and parsing stops at the first error or at `-h`/`--help`. Two flags
//! act while they are read, because what they do has to happen at the
//! process boundary: `--profile` latches the profile before anything can read
//! it, and `--join-url` is validated (and the join watchdog armed) so that a
//! bad link ends the launch with a sentence before anything else runs. Both
//! go through [`Effects`], so the parser itself can be tested without them.

use std::process::ExitCode;

use intoxicated_runtime::deeplink::JoinUrl;

/// Exit status of a parse error, `--help` and a refused `--check-join-url`.
pub const EXIT_USAGE: u8 = 2;
/// Exit status of a refused profile: busy, or its root could not be moved.
pub const EXIT_PROFILE: u8 = 3;

/// What the command line asked for.
pub struct Options {
    /// `--lib-dir`: the directory with the APK's `lib/<abi>/` objects, the
    /// linker's search path. Made absolute once the run directory is set up.
    pub lib_dir: String,
    /// `--library`: the object to `dlopen`.
    pub library: String,
    /// `--apk`: the APK assets are served from. Made absolute with `lib_dir`.
    pub apk: Option<String>,
    /// `--read-asset`: one asset to read through the AAsset API.
    pub read_asset: Option<String>,
    /// `--client-settings`: a file of flag names, one per line, read when the
    /// delivery is planned; an unreadable one means the built-in list.
    pub client_settings: Option<String>,
    /// `--flag-overrides`: the file's whole content, read while parsing.
    pub flag_overrides: Option<String>,
    /// `--gl-probe`.
    pub gl_probe: bool,
    /// `--window <secs>`.
    pub window: Option<u64>,
    /// `--profile`: the name, already latched through [`Effects::set_profile`].
    pub profile: Option<String>,
    /// `--host-libc`: accepted and handed to the symbol table builder.
    pub host_libc: bool,
    /// `--jni-onload`, also implied by `--game-activity` and `--dump-classes`.
    pub jni_onload: bool,
    /// `--game-activity`.
    pub game_activity: bool,
    /// `--join-url`, validated.
    pub join: Option<JoinUrl>,
    /// `--run <secs>`; 0 means no timer.
    pub run: u64,
    /// `--dump-classes <file>`.
    pub dump_classes: Option<String>,
    /// `-v`, `--verbose`.
    pub verbose: bool,
}

impl Options {
    fn new() -> Options {
        Options {
            lib_dir: String::new(),
            library: "libroblox.so".to_string(),
            apk: None,
            read_asset: None,
            client_settings: None,
            flag_overrides: None,
            gl_probe: false,
            window: None,
            profile: None,
            host_libc: false,
            jni_onload: false,
            game_activity: false,
            join: None,
            run: 15,
            dump_classes: None,
            verbose: false,
        }
    }
}

/// The parse's result when it did not fail.
pub enum Parsed {
    Run(Options),
    Help,
}

/// What two of the flags do while they are read.
pub trait Effects {
    /// Validate `name`, resolve its directory and make it this process's
    /// active profile. The message is the refusal.
    fn set_profile(&mut self, name: &str) -> Result<(), String>;
    /// A `--join-url` was accepted; called for every occurrence.
    fn join_requested(&mut self);
    /// The whole content of a `--flag-overrides` file.
    fn read_file(&mut self, path: &str) -> Result<String, String>;
}

/// The effects of a real launch.
pub struct Launch;

impl Effects for Launch {
    fn set_profile(&mut self, name: &str) -> Result<(), String> {
        intoxicated_runtime::profile::set_active(intoxicated_runtime::profile::dir(name)?)
    }

    fn join_requested(&mut self) {
        intoxicated_runtime::android::looper::note_join_requested();
    }

    fn read_file(&mut self, path: &str) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    }
}

/// Parse `args` (without the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I, effects: &mut dyn Effects) -> Result<Parsed, String> {
    let mut opt = Options::new();
    let mut lib_dir = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |missing: &str| args.next().ok_or_else(|| missing.to_string());
        match arg.as_str() {
            "--lib-dir" => lib_dir = Some(value("--lib-dir needs a value")?),
            "--library" => opt.library = value("--library needs a value")?,
            "--apk" => opt.apk = Some(value("--apk needs a path")?),
            "--read-asset" => opt.read_asset = Some(value("--read-asset needs a name")?),
            "--client-settings" => opt.client_settings = Some(value("--client-settings needs a path")?),
            "--flag-overrides" => {
                let path = value("--flag-overrides needs a path")?;
                opt.flag_overrides = Some(effects.read_file(&path)?);
            }
            "--gl-probe" => opt.gl_probe = true,
            "--window" => {
                let secs = value("--window needs a duration in seconds")?;
                opt.window = Some(secs.parse().map_err(|_| "--window wants a number".to_string())?);
            }
            "--profile" => {
                let name = value("--profile needs a name")?;
                effects.set_profile(&name)?;
                opt.profile = Some(name);
            }
            "--host-libc" => opt.host_libc = true,
            "--jni-onload" => opt.jni_onload = true,
            "--game-activity" => {
                opt.game_activity = true;
                opt.jni_onload = true;
            }
            "--join-url" => {
                let raw = value("--join-url needs a URL")?;
                let url = intoxicated_runtime::deeplink::validate(&raw)?;
                effects.join_requested();
                opt.join = Some(url);
            }
            "--run" => {
                let secs = value("--run needs a duration in seconds")?;
                opt.run = secs.parse().map_err(|_| "--run wants a number".to_string())?;
            }
            "--dump-classes" => {
                opt.dump_classes = Some(value("--dump-classes needs a path")?);
                opt.jni_onload = true;
            }
            "-v" | "--verbose" => opt.verbose = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }
    opt.lib_dir = lib_dir.ok_or_else(|| "--lib-dir is required".to_string())?;
    Ok(Parsed::Run(opt))
}

/// The process's own arguments, parsed with the launch's effects. A parse
/// error or `--help` has been reported on stderr when this returns `Err`.
pub fn from_process() -> Result<Options, ExitCode> {
    match parse(std::env::args().skip(1), &mut Launch) {
        Ok(Parsed::Run(opt)) => Ok(opt),
        Ok(Parsed::Help) => {
            eprint!("{USAGE}");
            Err(ExitCode::from(EXIT_USAGE))
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            eprint!("{USAGE}");
            Err(ExitCode::from(EXIT_USAGE))
        }
    }
}

/// `--check-join-url`: one link on stdin, the link the engine would be given
/// on stdout. Read from stdin so that a browser link's sign-in ticket is not
/// in this process's arguments. Nothing else runs.
pub fn check_join_url() -> ExitCode {
    use std::io::Read;
    let mut input = Vec::new();
    // A little over the link limit is enough to know a longer one is refused.
    if let Err(e) = std::io::stdin().take(64 * 1024).read_to_end(&mut input) {
        eprintln!("intoxicated: could not read the link from stdin: {e}");
        return ExitCode::from(2);
    }
    let text = String::from_utf8_lossy(&input);
    let link = text.strip_suffix('\n').unwrap_or(&text);
    match intoxicated_runtime::deeplink::validate(link) {
        Ok(url) => {
            println!("{}", url.delivered());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("intoxicated: {e}");
            ExitCode::from(2)
        }
    }
}

/// `--print-default-flags`: the built-in flag layer
/// ([`intoxicated_runtime::flag_layers::BUILT_IN`]) as one JSON object, keys
/// sorted, on stdout. Nothing else runs. The launcher's `--default-flags`
/// and Intoxicated Settings read it, so the defaults are kept in one place.
pub fn print_default_flags() -> ExitCode {
    println!("{}", default_flags_json());
    ExitCode::SUCCESS
}

/// The text [`print_default_flags`] prints, without its final newline.
pub fn default_flags_json() -> String {
    let layer: std::collections::BTreeMap<&str, &str> =
        intoxicated_runtime::flag_layers::BUILT_IN.iter().copied().collect();
    serde_json::to_string_pretty(&layer).expect("a map of strings always serialises")
}

/// `--print-ssr-looks`: the reflections' switch, knobs and named looks
/// ([`intoxicated_runtime::android::ssr::looks_json`]) as one JSON object on
/// stdout. Nothing else runs. The launcher's `--ssr-looks` runs it for
/// Intoxicated Settings' Shaders page, so the looks are kept in one place.
pub fn print_ssr_looks() -> ExitCode {
    println!("{}", intoxicated_runtime::android::ssr::looks_json());
    ExitCode::SUCCESS
}

/// `--print-shader-looks`: every effect drawn before the HUD (reflections,
/// depth of field, bloom), each with its switch, knobs and named looks
/// ([`intoxicated_runtime::android::ssr::shader_looks_json`]), as one JSON
/// object on stdout. Nothing else runs. The launcher's `--shader-looks` runs
/// it for Intoxicated Settings' Shaders page.
pub fn print_shader_looks() -> ExitCode {
    println!("{}", intoxicated_runtime::android::ssr::shader_looks_json());
    ExitCode::SUCCESS
}

/// The usage text, written to stderr by `--help` and after a parse error.
///
/// One block per option, the flag and its placeholder padded to column 20
/// and continuation lines indented to it; then one block per environment
/// variable, padded to column 37. Most of the variables are read by other
/// modules (the window, input, Vulkan, the deep link and the pump); the
/// block says what they do today and is kept in step with them.
pub const USAGE: &str = "\
usage: intoxicated-run --lib-dir <dir> [options]

  --lib-dir <dir>   directory holding the APK's lib/<abi>/ objects
  --library <name>  object to load (default: libroblox.so)
  --apk <path>      APK to serve assets from; without it AAssetManager_open fails
  --read-asset <p>  read one asset through the AAsset API and report its size
  --client-settings <f>  flag names, one per line, to send the engine instead
                    of the built-in list. Not the ClientSettings document: that
                    comes from Roblox's settings and <profile>/flags.json
  --flag-overrides <f>  JSON passed to nativePreloadFlagOverrides. DIAGNOSTIC
                    ONLY: that native does nothing observable despite its name,
                    tested with several document shapes. To actually set a flag,
                    put it in <profile>/flags.json
  --gl-probe        bring up GLES2 through the symbol table and read a pixel back
  --window <secs>   GL PROBE ONLY: open a window and draw a gradient for <secs>.
                    This is Intoxicated's own test pattern, not Roblox rendering.
  --profile <name>  which profile's storage, session and flags to run against;
                    the one named default when this is not given. One client
                    at a time per profile, held by a lock for the life of the
                    process; a second is refused (exit 3) rather than allowed
                    to write over the first one's Roblox storage
  --host-libc       also resolve libc from the host (ABI-unsafe; diagnostic only;
                    ignored on FreeBSD, where host libc is always consulted)
  --jni-onload      stand up a JavaVM and call JNI_OnLoad
  --game-activity   implies --jni-onload; bring Roblox up and hand it a surface
  --join-url <url>  join a place: a roblox:// or roblox-player:// link, the
                    roblox-player:1+launchmode:play+... link roblox.com hands
                    the browser, or an https roblox.com /games/<id> link. Each
                    is turned into roblox://experiences/start?placeId=<id>
                    (plus a server or private-server code it names) and
                    published once the home page is up. The browser link's
                    one-time gameinfo ticket is dropped and never printed. A
                    link that is not printable ASCII, is over 2048 bytes,
                    names no place or carries a malformed server or code
                    value is refused (exit 2) and nothing of it is delivered
  --check-join-url  read one link from stdin, print the link --join-url would
                    hand the engine and exit 0, or explain on stderr and exit
                    2. Runs nothing else; the launcher uses it
  --print-default-flags  print the flags the client sets itself (its
                    built-in layer, which <profile>/flags.json overrides) as
                    one JSON object, keys sorted, and exit 0. Runs nothing
                    else; the launcher's --default-flags runs it
  --print-ssr-looks  print the screen-space reflections' knobs (variable,
                    kind, range, default) and named looks (the value each
                    starts every knob at) as one JSON object, and exit 0.
                    Runs nothing else; the launcher's --ssr-looks runs it
  --print-shader-looks  the same for every effect drawn before the HUD
                    (reflections, depth of field, bloom), as one JSON object
                    with a list of effects, and exit 0. Runs nothing else;
                    the launcher's --shader-looks runs it
  --run <secs>      how long to let Roblox run after handover (default 15).
                    0 means no timer: run until the window is closed or the
                    process is sent SIGTERM/SIGINT. Closing the window ends the
                    process either way; the timer is a backstop for scripted
                    runs, not the way a session is meant to end
  --dump-classes <f>  implies --jni-onload; write the Java classes Roblox asked
                    for to <f>, the classes Intoxicated may still have to answer
  -v, --verbose     list every symbol and how it resolved

env:
  MCPELAUNCHER_LINKER_VERBOSITY=<n>  bionic linker tracing (try 1 or 2)
  INTOXICATED_STUB_ABORT=1           abort on the first unimplemented call
  INTOXICATED_STUB_QUIET=1           do not report stub hits as they happen
  INTOXICATED_TRACE=1                log libc calls: getenv and open64, the
                                     two names nothing else translates
  INTOXICATED_ANDROID_TRACE=1        log Android API calls (safe; no variadics)
  INTOXICATED_MONITOR=<n>            open the window centred on the nth
                                     Xinerama monitor (0 is the first) rather
                                     than at 0,0
  INTOXICATED_WINDOW_POS=<x>,<y>     explicit window position; wins over
                                     INTOXICATED_MONITOR
  INTOXICATED_FULLSCREEN=1           cover the chosen monitor and ask the
                                     window manager for fullscreen (F11 and
                                     devctl's fullscreen/windowed toggle it)
  INTOXICATED_SHOW_CURSOR=1          keep the host cursor over the window
                                     (hidden by default; the engine draws its
                                     own)
  INTOXICATED_RESOLUTION=<w>x<h>     render resolution (default 1280x720);
                                     a fullscreen monitor overrides it
  INTOXICATED_DPI_SCALE=<f>          UI density Roblox lays out against.
                                     1.0 is a low-density phone; try 1.5-2
  INTOXICATED_PLATFORM_NAME=<name>   what Intoxicated answers when the engine asks
                                     which platform it is on. Defaults to Linux,
                                     one of the engine's own Enum.Platform
                                     names; =Android is the control run
  INTOXICATED_WHEEL_SCALE=<f>        scroll wheel detents per notch (default 1);
                                     negative inverts the direction
  INTOXICATED_TRACE_WHEEL=1          log every wheel event and the arguments
                                     nativePassMouseWheel received
  INTOXICATED_TRACE_INPUT_LATENCY=1  time from each input entering the process
                                     (X event read, devctl line) to its engine
                                     call; summarised on stderr every 10 s and
                                     by devctl's `latency` verb
  INTOXICATED_NO_POINTER_LOCK=1      never capture the pointer, whatever the
                                     engine or the mouse asks for. The control
                                     for the capture path; it still polls and
                                     traces the engine's own request, so a
                                     control run says what it would have done
  INTOXICATED_NO_DRAG_LOCK=1         capture only when the engine asks, not
                                     while a right/middle button is held
  INTOXICATED_FORCE_POINTER_LOCK=1   hold the capture whenever the window has
                                     focus
  INTOXICATED_NO_CLOSE_EXIT=1        closing the window does not end the
                                     process, the control for the close path.
                                     SIGTERM, SIGINT and --run are unaffected.
                                     One SIGINT or SIGTERM ends the run
                                     through the normal shutdown (exit 0); a
                                     second one exits at once with 130
                                     (SIGINT) or 143 (SIGTERM)
  INTOXICATED_JOIN_TIMEOUT=<s>       with --join-url, seconds before the pump
                                     says no place has loaded (default 60)
  INTOXICATED_TEARDOWN_TIMEOUT_S=<s> how long the engine's shutdown may take
                                     before the process exits with 124
                                     (default 10; off disables it)
  INTOXICATED_POLL_COALESCE_US=<us>  the sleep before a zero-timeout ALooper
                                     poll after 64 empty ones in a row, once
                                     120 frames were presented (default 250;
                                     0 turns it off)
  INTOXICATED_DEEPLINK_RETRY=1       with --join-url, re-publish a link nothing
                                     answered within 15 s, once a second, at
                                     most 40 times, and only while nothing has
                                     launched. The launcher sets it
  INTOXICATED_DEEPLINK_NO_PUBLISH=1  with --join-url, hold the link but never
                                     publish it. The control: such a run stays
                                     on the home page
  INTOXICATED_DEEPLINK_PROBE=1       devctl's bussub prints each payload
                                     instead of its length (payloads can carry
                                     identities and private-server codes)
  INTOXICATED_WEBVIEW_TEST=<url>     publish one synthetic openWindow for <url>
                                     on the engine's bus once it is subscribed;
                                     a file: URL runs the path without opening
                                     anything
  INTOXICATED_NO_VULKAN=1            make the host look like it has no Vulkan
                                     loader, forcing the GLES2/EGL fallback
                                     path Roblox uses when dlopen(libvulkan)
                                     fails
  INTOXICATED_PRESENT_MODE=<m>       swapchain present mode: auto (the default:
                                     keep the engine's own choice, which is
                                     IMMEDIATE on this build, and replace only
                                     a vblank-paced FIFO with MAILBOX where the
                                     driver has it), off (forward the engine's
                                     choice untouched; the control), or one of
                                     mailbox, immediate, uncapped, fifo,
                                     fifo-relaxed. 'uncapped' means MAILBOX if
                                     the driver has it and IMMEDIATE otherwise.
                                     A mode the driver does not advertise falls
                                     back to what the engine asked for. FIFO and
                                     FIFO_RELAXED ran at about 1 frame per
                                     second on NVIDIA under Xorg here, vkcube
                                     included. A plugin can ask for one through
                                     the IntoxicatedPresentMode flag key; this
                                     variable overrules it
  INTOXICATED_ETC_EMULATION=<m>      auto (the default): on a GPU whose Vulkan
                                     driver lacks ETC2 (NVIDIA's does), report
                                     it and decode ETC2/EAC textures on the CPU,
                                     so the engine's ETC1 built-ins are not lost.
                                     off: the control; the engine then logs
                                     ETC1 0 ETC2 0. force: decode on the CPU even
                                     where the driver has ETC2, to compare the
                                     two. Test only
  INTOXICATED_ACQUIRE_SUBOPTIMAL=pass
                                     forward VK_SUBOPTIMAL_KHR from
                                     vkAcquireNextImageKHR to the engine instead
                                     of reporting it as VK_SUCCESS. The control
                                     for that substitution
  INTOXICATED_TEST_FAIL_PRESENT_MODES=<n>
                                     fail the first n present-mode queries with
                                     VK_ERROR_UNKNOWN and retry them on any GPU,
                                     to watch the NVIDIA retry work. Test only
  INTOXICATED_TRACE_PASSES=<n>       write one frame's render passes to stderr
                                     ([passes] lines): attachments, subpasses,
                                     draw counts, copies, barriers, submits.
                                     1 traces the frame after present 600, a
                                     larger number the frame after that
                                     present. With INTOXICATED_DEV_CONTROL the
                                     pass hooks are there too, for devctl's
                                     passes (trace the next frame) and
                                     depthshot <path> (the scene depth as a
                                     grayscale PNG, with a screenshot of the
                                     same frame and the raw floats beside it);
                                     without either they are not installed
  INTOXICATED_SSR=1                  screen-space reflections (experimental):
                                     wet-ground style reflections drawn into
                                     the 3D scene before the HUD, by compute
                                     passes of ours recorded into the engine's
                                     own command buffer. Off by default. With
                                     INTOXICATED_DEV_CONTROL, devctl's ssr
                                     on|off|stats|get|reset|look <name>|set
                                     <name> <value> changes them live and
                                     reports their GPU time. The looks below
                                     are the names set takes, in lower case;
                                     the defaults are a faint, soft look
                                     tuned on Timebomb Duels
  INTOXICATED_SSR_LOOK=<name>        start from a named look: competitive
                                     (the defaults), wet_road, clean_mirror,
                                     puddles or glossy; the variables below
                                     change it
  INTOXICATED_SSR_INTENSITY=<0..1>   how much reflection is blended in (0.4)
  INTOXICATED_SSR_ROUGHNESS=<0..1>   blur of the reflection, growing with
                                     distance (0.4; 0 is still slightly soft)
  INTOXICATED_SSR_FRESNEL=<0..1>     0 the same at every angle like a mirror,
                                     1 strong only at grazing angles (0.9)
  INTOXICATED_SSR_UP_ONLY=<0|1>      1 only surfaces facing up are wet, 0
                                     every surface (1)
  INTOXICATED_SSR_UP_THRESHOLD=<0..1>
                                     how upright a surface must be, as the
                                     cosine of its angle from up (0.6)
  INTOXICATED_SSR_MAX_DISTANCE=<x>   longest ray in multiples of the
                                     surface's distance from the camera; 0 no
                                     limit, sky included (0)
  INTOXICATED_SSR_EDGE_FADE=<0..0.5> fade at the screen edges, as a fraction
                                     of the smaller side (0.1)
  INTOXICATED_SSR_DARKEN=<0..1>      how much darker wet surfaces are (0)
  INTOXICATED_SSR_TINT=<r,g,b>       colour wet surfaces are multiplied by,
                                     each 0 to 2 (1,1,1)
  INTOXICATED_SSR_STEPS=<4..64>      steps along each ray (32)
  INTOXICATED_SSR_THICKNESS=<x>      how far behind a surface a ray still
                                     hits it, relative to its distance (0.06)
  INTOXICATED_SSR_FOV=<degrees>      the camera's vertical field of view the
                                     reflections assume (70, Roblox's default)
  INTOXICATED_SSR_KEEP_COLOUR=<0..1> 0 the reflection has the colours it
                                     reflects, 1 their brightness in the hue
                                     of the wet surface, so water stays blue
                                     (0.5)
  INTOXICATED_DOF=1                  depth of field (experimental): what is
                                     far from the focus is blurred, like a
                                     camera lens, before the HUD is drawn, so
                                     the HUD stays sharp. Off by default.
                                     devctl's dof takes the same words as ssr
  INTOXICATED_DOF_LOOK=<name>        cinematic (the defaults), subtle or
                                     portrait; the variables below change it
  INTOXICATED_DOF_STRENGTH=<0..32>   the largest blur, a radius in pixels at
                                     1080p (8)
  INTOXICATED_DOF_AUTOFOCUS=<0|1>    1 focus on the nearest thing around the
                                     screen centre, eased over time; 0 at
                                     FOCUS_DISTANCE (1)
  INTOXICATED_DOF_FOCUS_DISTANCE=<studs>
                                     the focus without autofocus (20)
  INTOXICATED_DOF_FOCUS_RANGE=<0..4> how far either side of the focus stays
                                     sharp, as a fraction of its distance
                                     (0.3)
  INTOXICATED_DOF_NEAR_BLUR=<0|1>    1 what is nearer than the focus blurs
                                     too (0)
  INTOXICATED_DOF_FOCUS_TIME=<secs>  how long the autofocus takes to settle
                                     (0.3)
  INTOXICATED_DOF_SAMPLES=<8..128>   taps of the blur (48)
  INTOXICATED_BLOOM=1                bloom (experimental): a glow around
                                     bright lights, drawn before the HUD, so
                                     the HUD never glows. Off by default.
                                     devctl's bloom takes the same words as
                                     ssr
  INTOXICATED_BLOOM_LOOK=<name>      soft (the defaults), neon or dreamy
  INTOXICATED_BLOOM_INTENSITY=<0..2> how strong the glow is (0.35)
  INTOXICATED_BLOOM_THRESHOLD=<0..1> the brightness a pixel starts to glow
                                     at (0.75)
  INTOXICATED_BLOOM_KNEE=<0..1>      how softly the glow starts around the
                                     threshold (0.5)
  INTOXICATED_BLOOM_RADIUS=<0..1>    0 a tight glow, 1 a wide one (0.7)
  INTOXICATED_BLOOM_TINT=<r,g,b>     colour of the glow, each 0 to 2 (1,1,1)
  INTOXICATED_FILES_DIR=<dir>        the engine's data root (default
                                     <profile>/data): its files/ and cache/
  INTOXICATED_EARLY_DIRS=<keys>      directory setters sent before
                                     initializeNativeCode: a comma list of
                                     files, cache, external and base (default
                                     files,cache; off for none, all for four)
  INTOXICATED_CHANNEL=<name>         the Roblox channel (default production)
  INTOXICATED_LATE_POST_MS=<ms>      wait after the surface handover before the
                                     late postClientSettingsLoaded and retry
                                     (default 250; off skips both)
  INTOXICATED_LATE_RETRY=off         skip the late nativeRetryInit
  INTOXICATED_SET_BASE_URL=<a>,<b>   call nativeSetBaseUrl(a, b) after the
                                     handover. The launcher sets it
  INTOXICATED_NOFILE=<n>             soft descriptor limit to raise to
                                     (default 8192, never above the hard one)
  INTOXICATED_RX_TEXT=off            leave the engine's text writable (the
                                     control for the read-only downgrade)
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the parser asked of the launch.
    #[derive(Default)]
    struct Recorded {
        profiles: Vec<String>,
        joins: usize,
        refuse_second_profile: bool,
    }

    impl Effects for Recorded {
        fn set_profile(&mut self, name: &str) -> Result<(), String> {
            if !intoxicated_runtime::profile::is_valid_name(name) {
                return intoxicated_runtime::profile::dir(name).map(|_| ());
            }
            if self.refuse_second_profile && self.profiles.first().is_some_and(|first| first != name) {
                return Err(format!("this instance already runs {}", self.profiles[0]));
            }
            self.profiles.push(name.to_string());
            Ok(())
        }
        fn join_requested(&mut self) {
            self.joins += 1;
        }
        fn read_file(&mut self, path: &str) -> Result<String, String> {
            std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
        }
    }

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn run(line: &str) -> Result<Options, String> {
        match parse(args(line), &mut Recorded::default())? {
            Parsed::Run(opt) => Ok(opt),
            Parsed::Help => Err("help".to_string()),
        }
    }

    fn error(line: &str) -> String {
        match run(line) {
            Ok(_) => panic!("{line:?} parsed"),
            Err(e) => e,
        }
    }

    #[test]
    fn the_launchers_arguments_parse() {
        let opt = run("--lib-dir /a/lib/x86_64 --apk /a/candidate-0.apk --host-libc --game-activity --run 0").unwrap();
        assert_eq!(opt.lib_dir, "/a/lib/x86_64");
        assert_eq!(opt.apk.as_deref(), Some("/a/candidate-0.apk"));
        assert!(opt.host_libc && opt.game_activity && opt.jni_onload);
        assert_eq!(opt.run, 0);
        assert_eq!(opt.library, "libroblox.so");
        assert!(opt.join.is_none() && opt.profile.is_none() && !opt.verbose);
    }

    #[test]
    fn defaults() {
        let opt = run("--lib-dir x").unwrap();
        assert_eq!(opt.run, 15);
        assert_eq!(opt.library, "libroblox.so");
        assert!(!opt.jni_onload && !opt.game_activity && !opt.gl_probe && opt.window.is_none());
        assert!(opt.apk.is_none() && opt.read_asset.is_none() && opt.dump_classes.is_none());
    }

    #[test]
    fn every_flag_is_read() {
        let opt = run(
            "--lib-dir l --library libx.so --apk a --read-asset r --client-settings c --gl-probe \
             --window 3 --host-libc --run 7 --dump-classes d -v",
        )
        .unwrap();
        assert_eq!(opt.library, "libx.so");
        assert_eq!(opt.read_asset.as_deref(), Some("r"));
        assert_eq!(opt.client_settings.as_deref(), Some("c"));
        assert!(opt.gl_probe && opt.host_libc && opt.verbose);
        assert_eq!(opt.window, Some(3));
        assert_eq!(opt.run, 7);
        assert_eq!(opt.dump_classes.as_deref(), Some("d"));
        assert!(opt.jni_onload, "--dump-classes implies --jni-onload");
        assert!(!opt.game_activity);
        assert!(run("--lib-dir l --verbose").unwrap().verbose);
        assert!(run("--lib-dir l --jni-onload").unwrap().jni_onload);
    }

    #[test]
    fn a_value_is_taken_whatever_it_looks_like_and_the_last_one_wins() {
        let opt = run("--lib-dir --help --run 5 --run 9").unwrap();
        assert_eq!(opt.lib_dir, "--help");
        assert_eq!(opt.run, 9);
        assert_eq!(run("--lib-dir a --lib-dir b").unwrap().lib_dir, "b");
    }

    #[test]
    fn the_error_messages() {
        assert_eq!(error(""), "--lib-dir is required");
        assert_eq!(error("--run 3"), "--lib-dir is required");
        assert_eq!(error("--bogus"), "unrecognised argument: --bogus");
        assert_eq!(error("--lib-dir x --check-join-url"), "unrecognised argument: --check-join-url");
        assert_eq!(error("--lib-dir x --print-default-flags"), "unrecognised argument: --print-default-flags");
        assert_eq!(error("--lib-dir x --print-ssr-looks"), "unrecognised argument: --print-ssr-looks");
        assert_eq!(error("--lib-dir x --print-shader-looks"), "unrecognised argument: --print-shader-looks");
        for (flag, message) in [
            ("--lib-dir", "--lib-dir needs a value"),
            ("--library", "--library needs a value"),
            ("--apk", "--apk needs a path"),
            ("--profile", "--profile needs a name"),
            ("--read-asset", "--read-asset needs a name"),
            ("--flag-overrides", "--flag-overrides needs a path"),
            ("--client-settings", "--client-settings needs a path"),
            ("--window", "--window needs a duration in seconds"),
            ("--run", "--run needs a duration in seconds"),
            ("--join-url", "--join-url needs a URL"),
            ("--dump-classes", "--dump-classes needs a path"),
        ] {
            assert_eq!(error(&format!("--lib-dir x {flag}")), message);
        }
        assert_eq!(error("--lib-dir x --run abc"), "--run wants a number");
        assert_eq!(error("--lib-dir x --run -1"), "--run wants a number");
        assert_eq!(error("--lib-dir x --window z"), "--window wants a number");
        assert_eq!(
            error("--lib-dir x --profile ../evil"),
            "\"../evil\" is not a usable profile name; use letters, digits, - and _"
        );
        assert_eq!(
            error("--lib-dir x --flag-overrides /nonexistent/f.json"),
            "/nonexistent/f.json: No such file or directory (os error 2)"
        );
        assert_eq!(
            error("--lib-dir x --join-url http://example.com/"),
            intoxicated_runtime::deeplink::validate("http://example.com/").unwrap_err()
        );
    }

    #[test]
    fn parsing_stops_at_the_first_error_or_at_help() {
        assert!(matches!(parse(args("--help --bogus"), &mut Recorded::default()), Ok(Parsed::Help)));
        assert!(matches!(parse(args("-h"), &mut Recorded::default()), Ok(Parsed::Help)));
        assert!(parse(args("--bogus --help"), &mut Recorded::default()).is_err());
        // Help is answered before --lib-dir is missed.
        assert!(matches!(parse(args("--run 2 --help"), &mut Recorded::default()), Ok(Parsed::Help)));
    }

    #[test]
    fn the_profile_is_latched_while_parsing() {
        let mut rec = Recorded { refuse_second_profile: true, ..Recorded::default() };
        let opt = match parse(args("--profile alt --lib-dir x --profile alt"), &mut rec).unwrap() {
            Parsed::Run(opt) => opt,
            Parsed::Help => unreachable!(),
        };
        assert_eq!(opt.profile.as_deref(), Some("alt"));
        assert_eq!(rec.profiles, ["alt", "alt"]);
        let mut rec = Recorded { refuse_second_profile: true, ..Recorded::default() };
        let e = parse(args("--lib-dir x --profile a --profile b"), &mut rec).err().unwrap();
        assert!(e.starts_with("this instance already runs"), "{e}");
    }

    #[test]
    fn every_join_url_is_validated_and_arms_the_watchdog() {
        let mut rec = Recorded::default();
        let line = "--lib-dir x --join-url roblox://experiences/start?placeId=1 \
                    --join-url https://www.roblox.com/games/5938036553/Some-Name";
        let opt = match parse(args(line), &mut rec).unwrap() {
            Parsed::Run(opt) => opt,
            Parsed::Help => unreachable!(),
        };
        assert_eq!(rec.joins, 2);
        assert_eq!(opt.join.unwrap().delivered(), "roblox://experiences/start?placeId=5938036553");
        let mut rec = Recorded::default();
        assert!(parse(args("--lib-dir x --join-url https://example.com/"), &mut rec).is_err());
        assert_eq!(rec.joins, 0, "a refused link arms nothing");
    }

    #[test]
    fn flag_overrides_are_read_at_parse_time() {
        let dir = std::env::temp_dir().join(format!("intoxicated-run-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("overrides.json");
        std::fs::write(&file, "{\"FFlagX\":\"True\"}").unwrap();
        let opt = run(&format!("--lib-dir x --flag-overrides {}", file.display())).unwrap();
        assert_eq!(opt.flag_overrides.as_deref(), Some("{\"FFlagX\":\"True\"}"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_flags_are_the_built_in_layer_with_sorted_keys() {
        let text = default_flags_json();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let object = doc.as_object().expect("one JSON object");
        let built_in = intoxicated_runtime::flag_layers::BUILT_IN;
        assert_eq!(object.len(), built_in.len());
        for (key, value) in built_in {
            assert_eq!(object[key], serde_json::Value::String(value.to_string()), "{key}");
        }
        // Sorted in the text itself, not only once parsed.
        let mut at = Vec::new();
        for (key, _) in built_in {
            at.push((text.find(&format!("\"{key}\"")).unwrap(), key));
        }
        at.sort();
        let order: Vec<&str> = at.iter().map(|(_, k)| *k).collect();
        let mut sorted: Vec<&str> = built_in.iter().map(|(k, _)| *k).collect();
        sorted.sort();
        assert_eq!(order, sorted);
    }

    #[test]
    fn the_exit_statuses() {
        assert_eq!(EXIT_USAGE, 2);
        assert_eq!(EXIT_PROFILE, 3);
    }

    #[test]
    fn the_usage_text_has_its_shape() {
        assert!(USAGE.starts_with("usage: intoxicated-run --lib-dir <dir> [options]\n\n"));
        assert!(USAGE.ends_with('\n') && !USAGE.ends_with("\n\n"));
        assert!(USAGE.is_ascii(), "no dashes or symbols outside ASCII");
        let (options, env) = USAGE.split_once("\n\nenv:\n").expect("an env: block");
        let options: Vec<&str> = options.lines().skip(2).collect();
        assert!(!options.is_empty());
        for line in &options {
            if line.starts_with("  -") {
                // The flag and its placeholder, then the text at column 20,
                // or two spaces after a flag too long for it.
                let text_at = line[2..].find("  ").map(|i| i + 2).expect(line);
                let text = line[text_at..].trim_start();
                let column = line.len() - text.len();
                assert!(column == 20 || (text_at > 18 && column == text_at + 2), "{line:?}");
            } else {
                assert!(line.starts_with(&" ".repeat(20)) && !line.starts_with(&" ".repeat(21)), "{line:?}");
            }
        }
        for line in env.lines() {
            if line.starts_with("  ") && !line.starts_with("   ") {
                let name = line[2..].split(' ').next().unwrap();
                assert!(name.contains('='), "{line:?}");
                let rest = &line[2 + name.len()..];
                assert!(rest.is_empty() || line.len() - rest.trim_start().len() == 37, "{line:?}");
            } else {
                assert!(line.starts_with(&" ".repeat(37)) && !line.starts_with(&" ".repeat(38)), "{line:?}");
            }
        }
    }

    #[test]
    fn the_usage_text_names_every_flag_and_no_stale_switch() {
        for flag in [
            "--lib-dir <dir>",
            "--library <name>",
            "--apk <path>",
            "--read-asset <p>",
            "--client-settings <f>",
            "--flag-overrides <f>",
            "--gl-probe",
            "--window <secs>",
            "--profile <name>",
            "--host-libc",
            "--jni-onload",
            "--game-activity",
            "--join-url <url>",
            "--check-join-url",
            "--print-default-flags",
            "--print-ssr-looks",
            "--print-shader-looks",
            "--run <secs>",
            "--dump-classes <f>",
            "-v, --verbose",
        ] {
            assert!(USAGE.contains(&format!("\n  {flag} ")), "{flag}");
        }
        for stale in ["WARNING", "variadic functions", "CONTRIBUTING"] {
            assert!(!USAGE.contains(stale), "{stale}");
        }
    }
}
