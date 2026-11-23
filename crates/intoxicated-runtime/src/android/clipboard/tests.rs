use super::*;
use crate::android::input::{Config, Core};
use std::ffi::CString;
use std::sync::atomic::AtomicU32;

/// A focused box backed by `input.rs`'s real editor, and a clipboard that can
/// be told to fail.
struct Fake {
    core: Core,
    focus: Option<(i64, u32)>,
    masked: bool,
    clipboard: Option<String>,
    refuse_clipboard: bool,
    edits: usize,
    delivered: Vec<(i64, FieldState)>,
}

impl Fake {
    /// Box 7, focus generation 1, holding `text` with the caret at the end.
    fn with(text: &str) -> Fake {
        let mut core = Core::new(Config {
            agdk_mouse: false,
            agdk_key: false,
            pass_key: true,
            keys_while_typing: false,
            wheel_scale: 1.0,
        });
        core.reseed(1, || (text.to_string(), None));
        Fake {
            core,
            focus: Some((7, 1)),
            masked: false,
            clipboard: None,
            refuse_clipboard: false,
            edits: 0,
            delivered: Vec::new(),
        }
    }

    fn last_text(&self) -> Option<&str> {
        self.delivered.last().map(|(_, s)| s.text.as_str())
    }
}

impl Desk for Fake {
    fn focused(&self) -> Option<(i64, u32)> {
        self.focus
    }
    fn masked(&self) -> bool {
        self.masked
    }
    fn selected_text(&mut self) -> Option<String> {
        self.core.selected_text()
    }
    fn edit(&mut self, edit: Edit<'_>) -> Option<FieldState> {
        self.edits += 1;
        self.core.edit(edit)
    }
    fn deliver(&mut self, which: i64, state: &FieldState) {
        self.delivered.push((which, state.clone()));
    }
    fn put_on_clipboard(&mut self, text: &str) -> Result<(), String> {
        if self.refuse_clipboard {
            return Err("the X server did not make Intoxicated the CLIPBOARD owner".into());
        }
        self.clipboard = Some(text.to_string());
        Ok(())
    }
}

fn member(name: &str, text: &str) -> (Source, String) {
    (Source::Member(name.into()), text.into())
}

// ---------------------------------------------------------------- extraction

#[test]
fn extraction_follows_the_member_rules() {
    let ok = |p: &str| extract(p).unwrap();
    assert_eq!(ok(r#"{"content":"hello","text":"wrong"}"#), member("content", "hello"));
    assert_eq!(ok(r#"{"kind":"text","text":"hello"}"#), member("text", "hello"));
    assert_eq!(ok(r#"{"somethingNobodyPredicted":"hello","count":3}"#), member("somethingNobodyPredicted", "hello"));
    assert_eq!(ok(r#"{"content":5,"text":"x"}"#), member("text", "x"));
    assert_eq!(ok(r#"{"value":"v","data":"d"}"#), member("value", "v"));
    assert_eq!(ok(r#"{"data":"d","clipboardText":"c"}"#), member("clipboardText", "c"));
    let (source, text) = ok(r#""bare""#);
    assert_eq!((source.label(), text.as_str()), ("(the whole payload)", "bare"));
    assert_eq!(ok(r#"{"content":""}"#), member("content", ""));
    assert_eq!(ok(r#"{"content":"a\nbü"}"#), member("content", "a\nb\u{fc}"));
}

#[test]
fn extraction_errors_name_members_and_kinds_but_never_values() {
    let e = extract(r#"{"a":"one","b":"two"}"#).unwrap_err();
    assert!(e.contains("a (a string)") && e.contains("b (a string)"), "{e}");
    assert!(!e.contains("one") && !e.contains("two"), "{e}");

    let e = extract("{}").unwrap_err();
    assert!(e.contains("no members"), "{e}");

    assert_eq!(extract("[1,2]").unwrap_err(), "the payload is an array and carries no members");
    assert_eq!(extract("null").unwrap_err(), "the payload is null and carries no members");
    assert_eq!(extract("true").unwrap_err(), "the payload is a boolean and carries no members");
    assert_eq!(extract("3").unwrap_err(), "the payload is a number and carries no members");

    let e = extract("hunter2").unwrap_err();
    assert!(e.contains("7 bytes") && !e.contains("hunter2"), "{e}");

    let e = extract(r#"{"a":"hunter2","b":"s3cret"}"#).unwrap_err();
    assert!(e.contains("a (") && e.contains("b ("), "{e}");
    assert!(!e.contains("hunter2") && !e.contains("s3cret"), "{e}");

    let e = extract(r#"{"n":1,"o":{"text":"inner"},"l":[]}"#).unwrap_err();
    assert!(e.contains("n (a number)") && e.contains("o (an object)") && e.contains("l (an array)"), "{e}");
    assert!(!e.contains("inner"), "{e}");
}

#[test]
fn the_sink_takes_65536_bytes_and_refuses_65537() {
    let payload = |text: &str| format!("{{\"content\":\"{text}\"}}");
    let at = receive(payload(&"x".repeat(MAX_BYTES)).as_bytes()).unwrap();
    assert_eq!((at.text.len(), at.source.label()), (65536, "content"));

    let e = receive(payload(&"x".repeat(MAX_BYTES + 1)).as_bytes()).unwrap_err();
    assert_eq!(e, "[clipboard] refusing 65537 bytes from the engine; the limit is 65536");
    // The limit is bytes, not characters.
    let e = receive(payload(&"\u{fc}".repeat(MAX_BYTES / 2 + 1)).as_bytes()).unwrap_err();
    assert!(e.contains("65538 bytes"), "{e}");
}

#[test]
fn the_sink_refuses_what_it_cannot_use_without_quoting_it() {
    assert_eq!(
        receive(&[b'"', 0xff, 0xfe, b'"']).unwrap_err(),
        "[clipboard] the engine published something that is not UTF-8; ignored"
    );
    let e = receive(b"hunter2").unwrap_err();
    assert!(e.starts_with("[clipboard] cannot use the engine's setClipboardText payload: the payload is not JSON (7 bytes)"), "{e}");
    assert!(!e.contains("hunter2"), "{e}");
    // A null payload arrives as nothing at all.
    assert!(receive(b"").unwrap_err().contains("0 bytes"));
}

#[test]
fn a_copied_value_does_not_show_its_text_when_debug_printed() {
    let copied = receive(br#"{"content":"hunter2"}"#).unwrap();
    let shown = format!("{copied:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(shown.contains("content") && shown.contains('7'), "{shown}");
    let whole = receive(br#""hunter2""#).unwrap();
    assert!(!format!("{whole:?}").contains("hunter2"));
    assert_eq!(whole.source.to_string(), "the whole payload");
    assert_eq!(copied.source.to_string(), "the payload's \"content\" member");
}

#[test]
fn the_probe_publishes_its_text_json_escaped_under_content() {
    let text = "say \"hi\"\n\\ \u{e9}";
    let payload = probe_payload(text);
    assert_eq!(payload, "{\"content\":\"say \\\"hi\\\"\\n\\\\ \u{e9}\"}");
    assert_eq!(extract(&payload).unwrap(), member("content", text));
}

#[test]
fn only_the_clipboard_topic_is_private() {
    assert!(is_private_topic(MESSAGE_ID));
    assert!(!is_private_topic("Game.launch"));
    assert!(!is_private_topic(""));
}

// ---------------------------------------------------------------- bus routing

/// The clipboard's sink and another listener on the same topic (devctl's
/// `bussub`) both hear a copy; a listener on another topic (the join link's
/// `Game.launch`) hears only its own. Through `bus.rs`'s real fan-out, with
/// the engine call stood in for.
#[test]
fn the_clipboard_topic_fans_out_beside_other_listeners() {
    static OTHER: AtomicU32 = AtomicU32::new(0);
    static LAUNCH: AtomicU32 = AtomicU32::new(0);
    extern "C" fn other(_: *const c_char) {
        OTHER.fetch_add(1, Ordering::SeqCst);
    }
    extern "C" fn launch(_: *const c_char) {
        LAUNCH.fetch_add(1, Ordering::SeqCst);
    }

    let mut clip = None;
    let joined = bus::subscribe_with(MESSAGE_ID, Some(on_set_clipboard_text), |t| {
        clip = Some(t);
        Ok(())
    });
    assert_eq!(joined, Ok(bus::Joined::Subscribed));
    let joined = bus::subscribe_with(MESSAGE_ID, Some(other), |_| panic!("the engine was asked twice for one id"));
    assert_eq!(joined, Ok(bus::Joined::Added));
    let mut game = None;
    let joined = bus::subscribe_with("test.clipboard.GameLaunch", Some(launch), |t| {
        game = Some(t);
        Ok(())
    });
    assert_eq!(joined, Ok(bus::Joined::Subscribed));
    let (clip, game) = (clip.unwrap(), game.unwrap());
    assert_ne!(clip as usize, game as usize, "two ids, two trampolines");

    let deliver = |to: bus::Sink, payload: &str| {
        let payload = CString::new(payload).unwrap();
        to(payload.as_ptr());
    };
    deliver(clip, r#"{"content":"from the engine"}"#);
    assert_eq!((OTHER.load(Ordering::SeqCst), LAUNCH.load(Ordering::SeqCst)), (1, 0));
    let parked = take_parked().expect("the sink parked the text");
    assert_eq!((parked.text.as_str(), parked.source.label()), ("from the engine", "content"));
    assert!(take_parked().is_none(), "taken once");

    deliver(game, r#"{"placeId":5938036553}"#);
    assert_eq!((OTHER.load(Ordering::SeqCst), LAUNCH.load(Ordering::SeqCst)), (1, 1));
    assert!(take_parked().is_none(), "a launch parks nothing");

    // A newer copy replaces one the pump has not taken yet.
    deliver(clip, r#"{"text":"one"}"#);
    deliver(clip, r#""two""#);
    let parked = take_parked().unwrap();
    assert_eq!((parked.text.as_str(), &parked.source), ("two", &Source::Whole));
    // A payload the sink refuses parks nothing, and the other listener
    // still hears it.
    deliver(clip, "[1,2]");
    assert!(take_parked().is_none());
    assert_eq!(OTHER.load(Ordering::SeqCst), 4);
}

// ---------------------------------------------------------------- the text box

#[test]
fn a_paste_flattens_control_characters_into_spaces() {
    let mut f = Fake::with("");
    assert_eq!(paste_text(&mut f, "a\nb\tc"), Ok(5));
    assert_eq!(f.delivered.len(), 1);
    assert_eq!(f.delivered[0].0, 7);
    assert_eq!(f.last_text(), Some("a b c"));
    assert_eq!(flatten("x\r\ny\u{7f}z"), "x  y z");
}

#[test]
fn a_paste_with_no_focused_box_returns_0_and_changes_nothing() {
    let mut f = Fake::with("kept");
    f.focus = None;
    assert_eq!(paste_text(&mut f, "anything"), Ok(0));
    assert_eq!((f.edits, f.delivered.len()), (0, 0));
}

#[test]
fn a_paste_replaces_the_selection_and_counts_characters_not_bytes() {
    let mut f = Fake::with("hello world");
    select_all(&mut f, 7);
    assert_eq!(f.last_text(), Some("hello world"));
    assert_eq!(paste_text(&mut f, "Gr\u{fc}\u{df}e"), Ok(5));
    let state = &f.delivered.last().unwrap().1;
    assert_eq!((state.text.as_str(), state.caret, state.sel_start, state.sel_end), ("Gr\u{fc}\u{df}e", 5, 5, 5));
}

#[test]
fn a_paste_over_the_limit_or_of_nothing_is_an_error_and_changes_nothing() {
    let mut f = Fake::with("kept");
    let e = paste_text(&mut f, &"x".repeat(MAX_BYTES + 1)).unwrap_err();
    assert_eq!(e, "the host clipboard holds 65537 bytes and the limit is 65536");
    assert!(paste_text(&mut f, "").is_err());
    assert_eq!((f.edits, f.delivered.len()), (0, 0));
    assert_eq!(paste_text(&mut f, &"x".repeat(MAX_BYTES)), Ok(MAX_BYTES));
}

#[test]
fn a_masked_box_takes_a_paste() {
    let mut f = Fake::with("");
    f.masked = true;
    assert_eq!(paste_text(&mut f, "pw"), Ok(2));
}

#[test]
fn a_late_answer_goes_only_into_the_box_it_was_asked_for() {
    let asked = PasteFor { handle: 0, which: 7, generation: 1 };
    let mut f = Fake::with("");
    assert_eq!(finish_paste(&mut f, asked, Ok("x\ty".into())), Ok(Some(3)));
    assert_eq!(f.last_text(), Some("x y"));

    // Blurred and refocused (the generation moved on), or another box.
    let mut f = Fake::with("");
    f.focus = Some((7, 2));
    assert_eq!(finish_paste(&mut f, asked, Ok("x".into())), Ok(None));
    f.focus = Some((8, 1));
    assert_eq!(finish_paste(&mut f, asked, Ok("x".into())), Ok(None));
    f.focus = None;
    assert_eq!(finish_paste(&mut f, asked, Ok("x".into())), Ok(None));
    assert_eq!((f.edits, f.delivered.len()), (0, 0));

    // The X side's errors pass through unchanged; size is checked again.
    let mut f = Fake::with("");
    let e = "the clipboard owner did not answer within 1000 ms".to_string();
    assert_eq!(finish_paste(&mut f, asked, Err(e.clone())), Err(e));
    assert!(finish_paste(&mut f, asked, Ok("x".repeat(MAX_BYTES + 1))).unwrap_err().contains("65537"));
    assert_eq!(f.delivered.len(), 0);
}

#[test]
fn copy_and_cut_check_in_order_and_print_one_line() {
    // Switched off comes first, even for a masked box.
    let mut f = Fake::with("secret");
    f.masked = true;
    select_all(&mut f, 7);
    assert_eq!(
        copy_or_cut(&mut f, 7, false, true),
        "[clipboard] copy not done: the clipboard bridge is off (INTOXICATED_SKIP_CLIPBOARD)"
    );
    assert_eq!(copy_or_cut(&mut f, 7, true, false), "[clipboard] cut refused: the focused box masks its text");
    assert_eq!(f.clipboard, None);

    let mut f = Fake::with("abc");
    assert_eq!(
        copy_or_cut(&mut f, 7, false, false),
        "[clipboard] copy: nothing is selected, so the clipboard is unchanged (Ctrl+A selects the whole box)"
    );
    assert_eq!(f.clipboard, None);

    let mut f = Fake::with(&"x".repeat(MAX_BYTES + 1));
    select_all(&mut f, 7);
    assert_eq!(copy_or_cut(&mut f, 7, false, false), "[clipboard] copy refused: 65537 bytes; the limit is 65536");
    assert_eq!(f.clipboard, None);
}

#[test]
fn copy_leaves_the_box_and_cut_empties_the_selection() {
    let mut f = Fake::with("h\u{e9}llo");
    select_all(&mut f, 7);
    let delivered = f.delivered.len();
    assert_eq!(copy_or_cut(&mut f, 7, false, false), "[clipboard] copy: 6 bytes onto the host clipboard");
    assert_eq!(f.clipboard.as_deref(), Some("h\u{e9}llo"));
    assert_eq!(f.delivered.len(), delivered, "a copy tells the engine nothing");

    assert_eq!(copy_or_cut(&mut f, 7, true, false), "[clipboard] cut: 6 bytes onto the host clipboard");
    assert_eq!(f.last_text(), Some(""));
    assert_eq!(f.delivered.len(), delivered + 1);
}

#[test]
fn a_cut_whose_copy_fails_leaves_the_field_unchanged() {
    let mut f = Fake::with("secret");
    select_all(&mut f, 7);
    let (edits, delivered) = (f.edits, f.delivered.len());
    f.refuse_clipboard = true;
    let line = copy_or_cut(&mut f, 7, true, false);
    assert_eq!(line, "[clipboard] cut failed: the X server did not make Intoxicated the CLIPBOARD owner");
    assert!(!line.contains("secret"));
    assert_eq!((f.edits, f.delivered.len()), (edits, delivered));
    assert_eq!(f.core.selected_text().as_deref(), Some("secret"), "still selected, still there");
}

// ------------------------------------------------------------ against X

// Seams in `native/jni_values.cpp` that focus and blur a text box the
// way the engine's `showKeyboard` does, for the live test.
extern "C" {
    fn intoxicated_textbox_focused(handle: i64, text: *const c_char, info: *const c_void);
    fn intoxicated_textbox_blurred();
}

/// The whole bridge against a real X server and real other clients, with no
/// engine: a box is focused through the native seam, and Ctrl+A, Ctrl+C and
/// Ctrl+V go through [`run_text_shortcut`] as `input.rs` calls it; the
/// engine's copy goes through the bus sink and [`pump_pending`]. Every read of
/// the clipboard is made by `xclip`, another process.
///
/// Ignored, and refuses to run without `INTOXICATED_CLIPBOARD_LIVE_TEST=1` or
/// on display `:0`, because it takes the display's clipboard:
///
/// ```text
/// Xvfb :97 -nolisten tcp &
/// DISPLAY=:97 INTOXICATED_CLIPBOARD_LIVE_TEST=1 cargo test -p intoxicated-runtime \
///   --release -- --ignored --test-threads=1 the_bridge_round_trips
/// ```
#[test]
#[ignore = "takes an X display's clipboard"]
fn the_bridge_round_trips_through_a_real_x_clipboard() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Instant;
    if std::env::var_os("INTOXICATED_CLIPBOARD_LIVE_TEST").is_none() {
        panic!("refusing to take a clipboard without INTOXICATED_CLIPBOARD_LIVE_TEST=1");
    }
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(display != ":0" && !display.is_empty(), "refusing DISPLAY={display:?}");

    super::super::x11_window::open(64, 64, "clipboard bridge probe").expect("an X display");
    let cb = host_clipboard().expect("the X11 clipboard");
    TEST_PASTES.lock().unwrap().clear();
    // What the pump does with selection events and the expiry.
    let pump = || {
        cb.serve_queued();
        pump_pending();
    };
    let xclip_out = || {
        let mut child = Command::new("xclip")
            .args(["-o", "-selection", "clipboard"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("xclip");
        let until = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && Instant::now() < until {
            pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let xclip_in = |text: &str| {
        let mut child = Command::new("xclip")
            .args(["-i", "-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("xclip");
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        pump();
    };

    // The engine copies: the payload arrives on the bus sink, the pump puts
    // it on the CLIPBOARD, xclip reads it.
    // Latin-1 and beyond (a Greek omega), so only UTF8_STRING carries it.
    let copied = "engine copy: Gr\u{fc}\u{df}e \u{3a9}";
    let payload = CString::new(probe_payload(copied)).unwrap();
    on_set_clipboard_text(payload.as_ptr());
    pump();
    let got = xclip_out();
    println!("engine -> X: xclip read {} bytes", got.len());
    assert_eq!(got, copied);

    // A box focused as showKeyboard focuses one; Ctrl+A then Ctrl+C.
    let typed = CString::new("typed in a box \u{e9}").unwrap();
    // SAFETY: `typed` outlives the call; a null spec is allowed.
    unsafe { intoxicated_textbox_focused(42, typed.as_ptr(), std::ptr::null()) };
    run_text_shortcut(0, 42, TextShortcut::SelectAll);
    run_text_shortcut(0, 42, TextShortcut::Copy);
    let got = xclip_out();
    println!("Ctrl+A, Ctrl+C -> X: xclip read {} bytes", got.len());
    assert_eq!(got, "typed in a box \u{e9}");

    // Another client copies; Ctrl+V returns before the owner answers and
    // the text lands in the box when the pump drains the answer.
    let pasted = "from xclip:\tGr\u{fc}\u{df}e";
    xclip_in(pasted);
    let revision = input::text_buffer_revision();
    let t0 = Instant::now();
    run_text_shortcut(0, 42, TextShortcut::Paste);
    let returned = t0.elapsed();
    let until = Instant::now() + Duration::from_secs(3);
    while input::text_buffer_revision() == revision {
        assert!(Instant::now() < until, "the paste never arrived");
        pump();
        std::thread::sleep(Duration::from_millis(1));
    }
    let arrived = t0.elapsed();
    let (text, caret) = input::text_buffer_peek();
    println!("Ctrl+V returned in {returned:?}; the text was in the box after {arrived:?}");
    assert_eq!(text, "from xclip: Gr\u{fc}\u{df}e", "the selection is replaced, the tab is a space");
    assert_eq!(caret, text.chars().count());
    assert!(returned < Duration::from_millis(5), "Ctrl+V waited {returned:?}");

    // Ctrl+X: onto the clipboard, out of the box.
    run_text_shortcut(0, 42, TextShortcut::SelectAll);
    run_text_shortcut(0, 42, TextShortcut::Cut);
    assert_eq!(input::text_buffer_peek().0, "");
    assert_eq!(xclip_out(), "from xclip: Gr\u{fc}\u{df}e");

    // Our own copy pastes without a round trip; a blurred box takes nothing.
    run_text_shortcut(0, 42, TextShortcut::Paste);
    assert_eq!(input::text_buffer_peek().0, "from xclip: Gr\u{fc}\u{df}e");
    xclip_in("late");
    run_text_shortcut(0, 42, TextShortcut::Paste);
    // SAFETY: no arguments.
    unsafe { intoxicated_textbox_blurred() };
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(input::text_buffer_peek().0, "from xclip: Gr\u{fc}\u{df}e", "a blurred box took a paste");
    // Taken, so `x11_clipboard`'s live test in the same run starts empty.
    let results = std::mem::take(&mut *TEST_PASTES.lock().unwrap());
    assert_eq!(results.last().map(|r| r.as_deref()), Some(Ok("late")), "the answer came, and was dropped");

    // Take the clipboard back so the forked xclip exits.
    cb.set_text("done").unwrap();
}
