use super::*;
use std::ffi::CString;
use std::sync::atomic::Ordering::SeqCst;

/// Not a ticket. Nothing in any output may contain it.
const TICKET: &str = "SYNTHETIC-NOT-A-TICKET";
const L0: &str = "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGame&browserTrackerId=1&placeId=1818&isPlayTogetherGame=false";
const JOB: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

/// Tests that touch the module's process-wide state take this first.
static GLOBALS: Mutex<()> = Mutex::new(());

fn encode(url: &str) -> String {
    url.chars()
        .map(|c| match c {
            '%' => "%25".to_string(),
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            '?' => "%3F".to_string(),
            '&' => "%26".to_string(),
            '=' => "%3D".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// The synthetic desktop link D(L).
fn desktop(launcher_url: &str) -> String {
    format!(
        "roblox-player:1+launchmode:play+gameinfo:{TICKET}+placelauncherurl:{}+launchtime:1754179200000\
         +browsertrackerid:1+robloxLocale:en_us+gameLocale:en_us",
        encode(launcher_url)
    )
}

/// roblox.com's browser link in the order the site emits it.
fn browser_link(ticket: &str, launcher_query: &str) -> String {
    format!(
        "roblox-player:1+launchmode:play+gameinfo:{ticket}+launchtime:1763820000000+placelauncherurl:{}\
         +browsertrackerid:1234567890+robloxLocale:en_us+gameLocale:en_us+channel:+LaunchExp:InApp",
        encode(&format!("https://www.roblox.com/Game/PlaceLauncher.ashx?{launcher_query}"))
    )
}

fn delivered(raw: &str) -> String {
    validate(raw).unwrap_or_else(|e| panic!("{raw} was refused: {e}")).delivered().to_string()
}

fn refusal(raw: &str) -> String {
    match validate(raw) {
        Ok(url) => panic!("{raw} was accepted as {url:?}"),
        Err(e) => e,
    }
}

// ------------------------------------------------------------------ gate

#[test]
fn the_gate_refuses_in_order_and_says_why() {
    assert_eq!(gate("").unwrap_err(), "--join-url was given an empty string");
    assert_eq!(gate("roblox://experiences/start?placeId=1818").unwrap(), Scheme::Roblox);
    assert_eq!(gate("ROBLOX-PLAYER://placeId=1").unwrap(), Scheme::RobloxPlayer);
    assert_eq!(gate("roblox-player:1+launchmode:play").unwrap(), Scheme::RobloxPlayer);
    assert_eq!(gate("https://www.roblox.com/games/1818").unwrap(), Scheme::Https);
    for (link, scheme) in [
        ("http://roblox.com", "http"),
        ("robloxmobile://experiences/start?placeId=1", "robloxmobile"),
        ("file:///etc/passwd", "file"),
        ("javascript:alert(1)", "javascript"),
        ("a\"b\\c:x", "a\\\"b\\\\c"),
    ] {
        assert_eq!(
            gate(link).unwrap_err(),
            format!("--join-url has scheme \"{scheme}\"; Intoxicated handles roblox, roblox-player and https roblox.com links")
        );
    }
    assert_eq!(gate("placeId=1818").unwrap_err(), "--join-url has no scheme");
    let not_printable = |at: usize| format!("--join-url has a character that is not printable ASCII at byte {at}");
    assert_eq!(gate("roblox://placeId=1\n[deeplink] joined").unwrap_err(), not_printable(18));
    assert_eq!(gate("roblox://place\0id=1").unwrap_err(), not_printable(14));
    assert_eq!(gate("roblox://placeId=1 2").unwrap_err(), not_printable(18));
    assert_eq!(gate("roblox://placeId=1\u{e9}").unwrap_err(), not_printable(18));
    let long = format!("roblox://launchData={}", "a".repeat(2048));
    assert_eq!(gate(&long).unwrap_err(), "--join-url is 2068 bytes and the limit is 2048");
    let exact = format!("roblox://launchData={}", "a".repeat(2028));
    assert_eq!(exact.len(), 2048);
    assert!(gate(&exact).is_ok());
}

#[test]
fn a_link_of_exactly_the_limit_is_taken() {
    let head = "roblox://experiences/start?placeId=1818&launchData=";
    let link = format!("{head}{}", "a".repeat(LIMIT - head.len()));
    assert_eq!(delivered(&link), link);
    assert!(refusal(&format!("{link}a")).contains("the limit is 2048"));
}

// ----------------------------------------------------------- description

#[test]
fn a_description_has_names_and_a_length_and_nothing_else() {
    assert_eq!(
        describe("roblox://experiences/start?placeId=5938036553"),
        "roblox:// with placeId (45 bytes; values not shown)"
    );
    assert_eq!(describe("roblox://experiences/start?placeId=1818"), "roblox:// with placeId (39 bytes; values not shown)");
    assert_eq!(describe("roblox://"), "roblox:// (9 bytes, no parameters)");
    let secret = describe("roblox-player://placeId=1818&accessCode=SECRETVALUE");
    assert_eq!(secret, "roblox-player:// with placeId, accessCode (51 bytes; values not shown)");
    assert!(!secret.contains("SECRETVALUE") && !secret.contains("1818"));
    assert_eq!(
        describe("roblox://experiences/start?placeId=1818&gameInstanceId=00000000-0000-0000-0000-000000000000"),
        "roblox:// with placeId, gameInstanceId (91 bytes; values not shown)"
    );
    let d = desktop(L0);
    assert_eq!(d.len(), 316);
    let shown = describe(&d);
    assert_eq!(
        shown,
        "roblox-player:// with launchmode, gameinfo, placelauncherurl, launchtime, browsertrackerid, robloxLocale, \
         gameLocale (316 bytes; values not shown)"
    );
    for hidden in ["SYNTHETIC", "1818", "assetgame"] {
        assert!(!shown.contains(hidden), "{hidden} in {shown}");
    }
    let split = describe("roblox-player:1+gameinfo:AAAA+BBBB==+launchmode:play");
    assert_eq!(split, "roblox-player:// with gameinfo, launchmode (52 bytes; values not shown)");
    assert!(!split.contains("BBBB"));
    assert_eq!(describe("roblox://placeId=1\",\"evil\":\"yes"), "roblox:// with placeId (31 bytes; values not shown)");
    let web_link = "https://www.roblox.com/games/1818/Name?privateServerLinkCode=777";
    let web = describe(web_link);
    assert_eq!(web, format!("https:// with privateServerLinkCode ({} bytes; values not shown)", web_link.len()));
    assert!(!web.contains("roblox.com") && !web.contains("777"));
}

#[test]
fn names_that_could_carry_a_value_are_not_printed() {
    assert_eq!(describe("roblox://a/b=1&c=2"), "roblox:// with c (18 bytes; values not shown)");
    let long = format!("roblox://{}=1", "n".repeat(41));
    assert!(describe(&long).ends_with("no parameters)"));
    let repeated = "roblox://placeId=1&placeId=2&a=1&placeId=3";
    assert_eq!(
        describe(repeated),
        format!("roblox:// with placeId, a, placeId ({} bytes; values not shown)", repeated.len())
    );
}

// ------------------------------------------------------- the desktop link

#[test]
fn the_desktop_link_is_translated_to_the_engine_grammar() {
    let url = validate(&desktop(L0)).unwrap();
    assert_eq!(url.delivered(), "roblox://experiences/start?placeId=1818");
    assert_eq!(url.place_id(), "1818");
    assert_eq!(url.dropped, ["request", "browserTrackerId", "isPlayTogetherGame"]);
    assert!(url.ticket);
    let same = "roblox://experiences/start?placeId=1818";
    assert_eq!(delivered(&desktop(L0).replacen("roblox-player:", "roblox-player:///", 1)), same);
    assert_eq!(delivered(&desktop(L0).replace("launchmode:play", "launchmode:PLAY")), same);
    assert_eq!(delivered(&format!("roblox-player:1+placelauncherurl:{}", encode(L0))), same);
    assert_eq!(delivered(&desktop("https://x/P.ashx?PLACEID=1818")), same);
    let twenty = "9".repeat(20);
    let link = delivered(&desktop(&format!("https://x/P.ashx?placeId={twenty}")));
    assert_eq!(link.len(), 55);
}

#[test]
fn the_real_browser_link_joins_its_place_and_drops_the_ticket() {
    let ticket = "FAKE-TICKET-4f2a9c1e7b3d";
    let link = browser_link(
        ticket,
        &format!(
            "request=RequestGame&browserTrackerId=1234567890&placeId=5938036553&isPlayTogetherGame=false\
             &joinAttemptId={JOB}&joinAttemptOrigin=PlayButton"
        ),
    );
    let url = validate(&link).unwrap();
    assert_eq!(url.delivered(), "roblox://experiences/start?placeId=5938036553");
    let mut everything = url.report();
    everything.push(format!("{url:?}"));
    everything.push(url.delivered().to_string());
    everything.push(detect_url_payload(url.delivered()));
    let (reply, lines) = dev_deeplink(&link, |json| {
        everything.push(json.to_string());
        Some(Ok(()))
    });
    assert_eq!(reply, "ok published");
    everything.extend(lines);
    for text in &everything {
        assert!(!text.contains(ticket), "the ticket reached {text}");
        assert!(!text.contains("FAKE"), "part of the ticket reached {text}");
    }
    assert!(everything.iter().any(|l| l.contains("sign-in ticket (gameinfo) is dropped")));
}

#[test]
fn a_server_or_private_server_in_the_browser_link_is_carried() {
    let job = browser_link(TICKET, &format!("request=RequestGameJob&browserTrackerId=1&placeId=1818&gameId={JOB}"));
    assert_eq!(delivered(&job), format!("roblox://experiences/start?placeId=1818&gameInstanceId={JOB}"));
    let private = browser_link(
        TICKET,
        "request=RequestPrivateGame&placeId=1818&accessCode=6e1a1d1e-0c4f-4b7e-9d3b-7a2f0e9c1b2a&linkCode=12345678901234567890",
    );
    assert_eq!(
        delivered(&private),
        "roblox://experiences/start?placeId=1818&accessCode=6e1a1d1e-0c4f-4b7e-9d3b-7a2f0e9c1b2a&linkCode=12345678901234567890"
    );
    for (key, name) in [("jobId", "gameInstanceId"), ("gameInstanceId", "gameInstanceId")] {
        assert_eq!(
            delivered(&desktop(&format!("https://x/P.ashx?placeId=1818&{key}={JOB}"))),
            format!("roblox://experiences/start?placeId=1818&{name}={JOB}")
        );
    }
    assert_eq!(
        delivered(&desktop("https://x/P.ashx?placeId=1818&reservedServerAccessCode=abc_DEF-1")),
        "roblox://experiences/start?placeId=1818&reservedServerAccessCode=abc_DEF-1"
    );
}

#[test]
fn malformed_server_values_refuse_the_link_without_naming_them() {
    for (query, says) in [
        ("placeId=1818&gameId=abc", "gameId is not a server id"),
        ("placeId=1818&jobId=0f1e2d3c4b5a69788796a5b4c3d2e1f0abcd", "gameId is not a server id"),
        ("placeId=1818&gameInstanceId=zz1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0", "gameId is not a server id"),
        ("placeId=1818&accessCode=a.b", "accessCode is not a private-server code"),
        ("placeId=1818&linkCode=", "linkCode is not a private-server code"),
        ("placeId=1818&reservedServerAccessCode=a%2Bb", "reservedServerAccessCode is not a private-server code"),
        ("placeId=1818&gameId=0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0&jobId=0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0", "more than once"),
        ("placeId=1818&placeId=1818", "names placeId more than once"),
    ] {
        let e = refusal(&desktop(&format!("https://x/P.ashx?{query}")));
        assert!(e.starts_with("--join-url was refused: "), "{e}");
        assert!(e.contains(says), "{query}: {e}");
        for value in ["abc", "a.b", "zz1e", "a%2Bb", TICKET] {
            assert!(!e.contains(value), "{e}");
        }
    }
    let long = "a".repeat(65);
    assert!(refusal(&desktop(&format!("https://x/P.ashx?placeId=1818&accessCode={long}"))).contains("accessCode"));
    assert_eq!(
        delivered(&desktop(&format!("https://x/P.ashx?placeId=1818&accessCode={}", "a".repeat(64)))),
        format!("roblox://experiences/start?placeId=1818&accessCode={}", "a".repeat(64))
    );
}

#[test]
fn a_desktop_link_that_cannot_be_translated_is_refused() {
    let says = |raw: &str, what: &str| {
        let e = refusal(raw);
        assert!(e.contains(what), "{raw}: {e}");
        assert!(!e.contains(TICKET), "{e}");
    };
    says(&desktop(L0).replace("launchmode:play", "launchmode:edit"), "launchmode");
    says(&desktop("https://x/PlaceLauncher.ashx?request=RequestFollowUser&userId=1"), "names no placeId");
    says(&desktop("https://x/P.ashx"), "names no placeId");
    let nines = "9".repeat(21);
    for bad in ["1818x", "1818%26accessCode%3Dabc", "", nines.as_str()] {
        says(&desktop(&format!("https://x/P.ashx?placeId={bad}")), "placeId is not a number");
    }
    says(&format!("roblox-player:1+launchmode:play+gameinfo:{TICKET}"), "placelauncherurl");
    says("roblox-player:1+launchmode:play+placelauncherurl:https%3A%2F%2Fx%3FplaceId%3D1818%2", "cut short");
    says("roblox-player:1+launchmode:play+placelauncherurl:https%3A%2Fx%3FplaceId%3D18%ZZ", "not two hexadecimal digits");
    says("roblox-player:1+placelauncherurl:https%3A%2F%2Fx%3FplaceId%3D1%0A", "not printable");
    says("roblox-player:1+placelauncherurl:https%3A%2F%2Fx%3FplaceId%3D1%00", "not printable");
}

#[test]
fn percent_decoding_happens_once_and_strictly() {
    assert_eq!(percent_decode("a%3Db%26c").unwrap(), "a=b&c");
    assert_eq!(percent_decode("%25").unwrap(), "%");
    assert_eq!(percent_decode("%2525").unwrap(), "%25");
    assert_eq!(percent_decode("plain+text").unwrap(), "plain+text");
    assert!(percent_decode("a%0Ab").is_err());
    assert!(percent_decode("a%00b").is_err());
    assert!(percent_decode("a%7Fb").is_err());
    assert!(percent_decode("a%2").is_err());
    assert!(percent_decode("a%").is_err());
}

// --------------------------------------------------------------- web links

#[test]
fn roblox_com_game_links_are_translated() {
    let same = "roblox://experiences/start?placeId=1818";
    for link in [
        "https://www.roblox.com/games/1818",
        "https://www.roblox.com/games/1818/",
        "https://www.roblox.com/games/1818/Some-Experience-Name",
        "https://WWW.ROBLOX.COM/GAMES/1818",
        "https://roblox.com/games/1818",
        "https://web.roblox.com/games/1818",
        "https://www.roblox.com/de/games/1818/Name",
        "https://www.roblox.com/en-us/games/1818",
        "https://www.roblox.com/games/1818#!/game-instances",
    ] {
        assert_eq!(delivered(link), same, "{link}");
    }
    let tracked = validate("https://www.roblox.com/games/1818/Name?ref=x&utm_source=y").unwrap();
    assert_eq!(tracked.delivered(), same);
    assert_eq!(tracked.dropped, ["ref", "utm_source"]);
    assert_eq!(
        delivered("https://www.roblox.com/games/1818/Name?privateServerLinkCode=09735419396436185419588656284364"),
        "roblox://experiences/start?placeId=1818&linkCode=09735419396436185419588656284364"
    );
    assert_eq!(
        delivered(&format!("https://www.roblox.com/games/start?placeId=1818&gameInstanceId={JOB}")),
        format!("roblox://experiences/start?placeId=1818&gameInstanceId={JOB}")
    );
}

#[test]
fn other_https_links_are_refused_without_naming_the_host() {
    for (link, says) in [
        ("https://evil.example/games/1818", "not to roblox.com"),
        ("https://www.roblox.com.evil.example/games/1818", "not to roblox.com"),
        ("https://user@www.roblox.com/games/1818", "not to roblox.com"),
        ("https://www.roblox.com:8443/games/1818", "not to roblox.com"),
        ("https:www.roblox.com/games/1818", "not a roblox.com /games/ link"),
        ("https://www.roblox.com/users/1/profile", "not a roblox.com /games/ link"),
        ("https://www.roblox.com/games/abc", "not a roblox.com /games/ link"),
        ("https://www.roblox.com/games/start/x?placeId=1", "not a roblox.com /games/ link"),
        ("https://www.roblox.com/share?code=abc&type=Server", "not a roblox.com /games/ link"),
        ("https://www.roblox.com/games/start?launchData=x", "names no placeId"),
        ("https://www.roblox.com/games/1818?privateServerLinkCode=bad.code", "privateServerLinkCode is not"),
        ("https://www.roblox.com/games/1818?privateServerLinkCode=1&privateServerLinkCode=1", "more than once"),
    ] {
        let e = refusal(link);
        assert!(e.contains(says), "{link}: {e}");
        assert!(!e.contains("evil") && !e.contains("bad.code") && !e.contains("user@"), "{e}");
    }
    assert!(refusal(&format!("https://www.roblox.com/games/{}", "9".repeat(21))).contains("not a number"));
    assert!(refusal("http://www.roblox.com/games/1818").contains("scheme \"http\""));
}

// --------------------------------------------------------- engine grammar

#[test]
fn engine_grammar_links_are_checked_and_passed_through() {
    for link in [
        "roblox://experiences/start?placeId=1818",
        "roblox://experiences/start?placeId=1818&accessCode=X",
        "roblox://experiences/start?placeId=1818&linkCode=abc-123_DEF",
        "roblox://experiences/start?placeId=1818&gameInstanceId=00000000-0000-0000-0000-000000000000",
        "roblox://experiences/start?placeId=1818&launchData=hello%20world",
        "roblox://experiences/start?placeid=1818",
        "roblox://placeId=1818",
        "roblox://experiences/start?placeId=1818&launchData=a?placeId=2",
    ] {
        let url = validate(link).unwrap();
        assert_eq!(url.delivered(), link);
        assert_eq!(url.place_id(), "1818");
        assert_eq!(url.report().len(), 1, "an engine link is delivered as it is: {:?}", url.report());
    }
    assert_eq!(delivered("ROBLOX://placeId=1"), "roblox://placeId=1");
    assert_eq!(delivered("roblox:placeId=1"), "roblox://placeId=1");
    assert_eq!(delivered("roblox:///experiences/start?placeId=1"), "roblox://experiences/start?placeId=1");
    assert_eq!(delivered("roblox-player://placeId=1818"), "roblox://placeId=1818");
    assert_eq!(delivered("ROBLOX-PLAYER://placeId=1"), "roblox://placeId=1");
    assert_eq!(validate("roblox://experiences/start?placeId=0042").unwrap().place_id(), "42");
}

#[test]
fn malformed_engine_grammar_links_are_refused() {
    for (link, says) in [
        ("roblox://", "names no placeId"),
        ("roblox://experiences/start", "names no placeId"),
        ("roblox://experiences/start?launchData=hello", "names no placeId"),
        ("roblox://experiences/start?placeId=12a", "placeId is not a number"),
        ("roblox://experiences/start?placeId=-5", "placeId is not a number"),
        ("roblox://placeId=1\",\"evil\":\"yes", "placeId is not a number"),
        ("roblox://experiences/start?placeId=1&placeId=2", "names placeId more than once"),
        ("roblox://experiences/start?placeId=1&gameInstanceId=abc", "gameInstanceId is not a server id"),
        ("roblox://experiences/start?placeId=1&accessCode=a%2Fb", "accessCode is not a private-server code"),
        ("roblox://experiences/start?placeId=1&linkCode=", "linkCode is not a private-server code"),
        ("roblox-player://placeId=1818&accessCode=SECRET.VALUE", "accessCode is not"),
    ] {
        let e = refusal(link);
        assert!(e.contains(says), "{link}: {e}");
        assert!(!e.contains("SECRET") && !e.contains("a%2Fb"), "{e}");
    }
}

#[test]
fn every_delivered_link_is_accepted_again_unchanged() {
    for link in [
        desktop(L0),
        browser_link(TICKET, &format!("request=RequestGameJob&placeId=1818&gameId={JOB}")),
        "https://www.roblox.com/games/1818/Name?privateServerLinkCode=777".to_string(),
        "roblox-player://placeId=1818".to_string(),
        "ROBLOX://experiences/start?placeId=1".to_string(),
    ] {
        let once = delivered(&link);
        assert_eq!(delivered(&once), once, "{link}");
        assert_eq!(validate(&once).unwrap().report().len(), 1);
    }
}

#[test]
fn the_report_names_what_happened() {
    let report = validate(&desktop(L0)).unwrap().report();
    assert_eq!(
        report,
        [
            format!("[deeplink] delivering {}", describe(&desktop(L0))),
            "[deeplink] translated the desktop link to roblox:// with placeId (39 bytes; values not shown)".to_string(),
            "[deeplink] its one-time sign-in ticket (gameinfo) is dropped; it never reaches the engine".to_string(),
            "[deeplink] its launcher URL also carried request, browserTrackerId, isPlayTogetherGame, which \
             Intoxicated does not carry across"
                .to_string(),
        ]
    );
    let player = validate("roblox-player://placeId=1818").unwrap().report();
    assert_eq!(player[1], "[deeplink] translated the roblox-player link to roblox:// with placeId (21 bytes; values not shown)");
    assert_eq!(format!("{:?}", validate("roblox://placeId=1818&accessCode=SECRET").unwrap()), "JoinUrl(roblox:// with placeId, accessCode (39 bytes; values not shown))");
}

// ---------------------------------------------------------------- payload

#[test]
fn the_detect_url_payload_is_exact() {
    assert_eq!(
        detect_url_payload("roblox://experiences/start?placeId=1818"),
        "{\"url\":\"roblox://experiences/start?placeId=1818\"}"
    );
    assert_eq!(detect_url_payload(&delivered(&desktop(L0))), "{\"url\":\"roblox://experiences/start?placeId=1818\"}");
    let hostile = detect_url_payload("roblox://placeId=1\",\"evil\":\"yes");
    assert_eq!(hostile, "{\"url\":\"roblox://placeId=1\\\",\\\"evil\\\":\\\"yes\"}");
    assert_eq!(hostile.matches("\":\"").count(), 1);
    assert_eq!(detect_url_payload("roblox://a\\"), "{\"url\":\"roblox://a\\\\\"}");
    assert_eq!(detect_url_payload("roblox://experiences/start?placeId=5938036553").len(), 55);
}

// -------------------------------------------------------- refusals deliver nothing

#[test]
fn a_refused_link_publishes_nothing() {
    for link in [
        String::new(),
        "http://roblox.com".into(),
        "roblox://experiences/start?launchData=x".into(),
        desktop(L0).replace("launchmode:play", "launchmode:edit"),
        desktop("https://x/P.ashx?placeId=1818&accessCode=a.b"),
        "https://evil.example/games/1818".into(),
        format!("roblox-player:1+launchmode:play+gameinfo:{TICKET}"),
    ] {
        let mut called = false;
        let (reply, lines) = dev_deeplink(&link, |_| {
            called = true;
            Some(Ok(()))
        });
        assert!(!called, "{link} reached the engine");
        assert!(reply.starts_with("err --join-url "), "{reply}");
        assert!(lines.is_empty());
        assert!(!reply.contains(TICKET), "{reply}");
    }
}

#[test]
fn a_devctl_link_is_translated_before_it_is_published() {
    let mut sent = Vec::new();
    let (reply, lines) = dev_deeplink(&desktop(L0), |json| {
        sent.push(json.to_string());
        Some(Ok(()))
    });
    assert_eq!(reply, "ok published");
    assert_eq!(sent, ["{\"url\":\"roblox://experiences/start?placeId=1818\"}"]);
    assert_eq!(lines.last().unwrap(), "[deeplink] (devctl) published Linking.detectURL");
    let (reply, _) = dev_deeplink("roblox://placeId=1", |_| Some(Err("no".into())));
    assert_eq!(reply, "err not published");
    let (reply, _) = dev_deeplink("roblox://placeId=1", |_| None);
    assert_eq!(reply, "err not published");
}

// ------------------------------------------------------------ several subscribers

#[test]
fn bussub_game_launch_joins_the_join_confirmation_instead_of_replacing_it() {
    let _g = GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
    take_launches();
    WORK.fetch_or(HELD_BIT, SeqCst);
    let mut engine_subscribes = 0;
    let mut trampoline = None;
    // The held link's subscription, as `deliver` makes it.
    bus::subscribe_with(GAME_LAUNCH, Some(on_game_launch), |t| {
        engine_subscribes += 1;
        trampoline = Some(t);
        Ok(())
    })
    .unwrap();
    // devctl's `bussub Game.launch`.
    let before = DEV_BUS_PAYLOADS.load(SeqCst);
    let reply = dev_bussub(GAME_LAUNCH, |id, sink| {
        Some(bus::subscribe_with(id, Some(sink), |_| {
            engine_subscribes += 1;
            Ok(())
        }))
    });
    assert_eq!(reply, "ok subscribed");
    assert_eq!(dev_bussub(GAME_LAUNCH, |id, sink| Some(bus::subscribe_with(id, Some(sink), |_| Ok(())))), "ok subscribed");
    assert_eq!(engine_subscribes, 1, "the engine subscription is made once");

    let payload =
        CString::new(format!("{{\"placeId\":5938036553,\"referralPage\":\"DeepLink\",\"joinAttemptId\":\"{JOB}\"}}"))
            .unwrap();
    (trampoline.unwrap())(payload.as_ptr());
    let launches = take_launches();
    WORK.fetch_and(!HELD_BIT, SeqCst);
    assert_eq!(launches.len(), 1, "the join confirmation still hears Game.launch");
    assert_eq!(launches[0].place, LaunchPlace::Place("5938036553".into()));
    assert_eq!(DEV_BUS_PAYLOADS.load(SeqCst), before + 1, "and so does devctl, once");
}

/// `bussub` on the clipboard's topic joins the clipboard bridge like any
/// other listener, but with the listener that never prints a payload.
#[test]
fn bussub_on_the_clipboard_topic_listens_without_showing_the_text() {
    let mut given = None;
    let reply = dev_bussub(crate::android::clipboard::MESSAGE_ID, |_, sink| {
        given = Some(sink as usize);
        Some(Ok(bus::Joined::Added))
    });
    assert_eq!(reply, "ok subscribed");
    assert_eq!(given, Some(on_dev_private_payload as bus::Sink as usize));
    let mut given = None;
    dev_bussub(GAME_LAUNCH, |_, sink| {
        given = Some(sink as usize);
        Some(Ok(bus::Joined::Added))
    });
    assert_eq!(given, Some(on_dev_bus_payload as bus::Sink as usize));
}

#[test]
fn bussub_replies() {
    assert_eq!(dev_bussub("", |_, _| panic!("called")), "err bussub <message id>");
    assert_eq!(dev_bussub("X.y", |_, _| None), "err doSubscribeRaw is not exported");
    assert_eq!(dev_bussub("X.y", |_, _| Some(Err("full".into()))), "err subscribe failed: full");
}

#[test]
fn a_launch_is_read_for_its_place() {
    assert_eq!(launch_place(b"{\"placeId\":5938036553,\"referralPage\":\"DeepLink\"}"), LaunchPlace::Place("5938036553".into()));
    assert_eq!(launch_place(b"{\"placeId\":\"0042\"}"), LaunchPlace::Place("42".into()));
    assert_eq!(launch_place(b"{\"placeId\":1.5}"), LaunchPlace::Unknown);
    assert_eq!(launch_place(b"{\"placeId\":-1}"), LaunchPlace::Unknown);
    assert_eq!(launch_place(b"{\"universeId\":1}"), LaunchPlace::Unknown);
    assert_eq!(launch_place(b"not json"), LaunchPlace::Unknown);
}

// ------------------------------------------------------------- the held link

#[derive(Default)]
struct Fake {
    published: Vec<String>,
    registered: bool,
}

impl Engine for Fake {
    fn publish(&mut self, json: &str) -> Option<Result<(), String>> {
        self.published.push(json.to_string());
        Some(Ok(()))
    }

    fn cold_start_to_game(&mut self) -> Option<Result<bool, String>> {
        Some(Ok(self.registered))
    }
}

fn machine(link: &str, live: bool, retry: bool) -> Machine {
    let mut m = Machine::new(&validate(link).unwrap(), 0);
    m.live = live;
    m.retry = retry;
    m
}

fn home() -> Inputs {
    Inputs { home_up: true, ..Inputs::default() }
}

fn launch(place: &str, after_publish: bool) -> Inputs {
    Inputs {
        home_up: true,
        launches: vec![Launch { place: LaunchPlace::Place(place.into()), after_publish }],
        ..Inputs::default()
    }
}

#[test]
fn only_a_launch_of_the_requested_place_confirms_the_link() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    assert!(m.step(t0, &Inputs::default(), &mut e).is_empty(), "nothing before the home page");
    assert_eq!(m.step(t0, &home(), &mut e), ["[deeplink] (home page interactive) published Linking.detectURL"]);
    assert_eq!(e.published, ["{\"url\":\"roblox://experiences/start?placeId=1818\"}"]);
    assert_eq!(m.step(t0, &launch("1818", true), &mut e), [SUCCESS]);
    assert!(m.done);
    assert!(m.step(t0 + Duration::from_secs(60), &home(), &mut e).is_empty());
    assert_eq!(e.published.len(), 1, "a confirmed link is never published again");

    let mut m = machine("roblox://experiences/start?placeId=0042", true, true);
    m.step(t0, &home(), &mut e);
    assert_eq!(m.step(t0, &launch("42", true), &mut e), [SUCCESS]);
}

#[test]
fn a_launch_of_another_place_does_not_confirm_and_stops_the_link() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    m.step(t0, &home(), &mut e);
    let lines = m.step(t0, &launch("189707", true), &mut e);
    assert_eq!(lines, [OTHER_PLACE]);
    assert!(!lines.iter().any(|l| l == SUCCESS));
    for s in 15..60 {
        assert!(m.step(t0 + Duration::from_secs(s), &home(), &mut e).is_empty());
    }
    assert_eq!(e.published.len(), 1, "no retry after a launch of another place");

    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    m.step(t0, &home(), &mut e);
    let unknown = Inputs {
        launches: vec![Launch { place: LaunchPlace::Unknown, after_publish: true }],
        ..home()
    };
    assert_eq!(m.step(t0, &unknown, &mut e), [UNKNOWN_PLACE]);
}

#[test]
fn a_launch_before_the_publish_means_the_link_is_never_published() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    assert_eq!(m.step(t0, &launch("1818", false), &mut e), [ALREADY_LAUNCHING]);
    assert!(m.step(t0, &home(), &mut e).is_empty());
    assert!(e.published.is_empty());
}

#[test]
fn the_retry_is_bounded_and_spaced() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    m.step(t0, &home(), &mut e);
    let mut at = Vec::new();
    let mut lines = Vec::new();
    let mut t = t0;
    while !m.done {
        t += Duration::from_millis(250);
        let before = e.published.len();
        lines.extend(m.step(t, &home(), &mut e));
        if e.published.len() > before {
            at.push(t);
        }
        assert!(t < t0 + Duration::from_secs(120), "never finished");
    }
    assert_eq!(e.published.len(), 41, "one publish and 40 retries");
    assert_eq!(at[0] - t0, VERDICT_WINDOW);
    assert!(at.windows(2).all(|w| w[1] - w[0] >= RETRY_INTERVAL));
    assert_eq!(lines.iter().filter(|l| l.starts_with("[deeplink] re-published the link (retry ")).count(), 40);
    assert!(lines.contains(&"[deeplink] re-published the link (retry 40/40)".to_string()));
    assert_eq!(lines.last().unwrap(), VERDICT_NOTHING);
    assert_eq!(t - *at.last().unwrap(), Duration::from_millis(250), "the verdict follows on the next tick");
}

#[test]
fn a_launch_during_the_retries_stops_them() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    m.step(t0, &home(), &mut e);
    m.step(t0 + VERDICT_WINDOW, &home(), &mut e);
    m.step(t0 + VERDICT_WINDOW + RETRY_INTERVAL, &home(), &mut e);
    assert_eq!(e.published.len(), 3);
    assert_eq!(m.step(t0 + VERDICT_WINDOW + RETRY_INTERVAL * 2, &launch("1818", true), &mut e), [SUCCESS]);
    assert_eq!(e.published.len(), 3);
}

#[test]
fn a_join_in_the_engine_log_stops_the_retries() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    m.step(t0, &home(), &mut e);
    let joined = Inputs { joins: 1, ..home() };
    assert_eq!(m.step(t0 + VERDICT_WINDOW, &joined, &mut e), [JOIN_UNDER_WAY]);
    assert_eq!(e.published.len(), 1);
}

#[test]
fn without_a_live_subscription_or_the_switch_there_is_no_retry() {
    let t0 = Instant::now();
    for (live, retry, verdict) in [(false, false, VERDICT_NO_SUBSCRIPTION), (true, false, VERDICT_NOTHING)] {
        let mut e = Fake::default();
        let mut m = machine("roblox://experiences/start?placeId=1818", live, retry);
        m.step(t0, &home(), &mut e);
        assert!(m.step(t0 + VERDICT_WINDOW - Duration::from_millis(1), &home(), &mut e).is_empty());
        assert_eq!(m.step(t0 + VERDICT_WINDOW, &home(), &mut e), [verdict]);
        assert_eq!(e.published.len(), 1);
    }
    let mut e = Fake { registered: true, ..Fake::default() };
    let mut m = machine("roblox://experiences/start?placeId=1818", true, false);
    m.step(t0, &home(), &mut e);
    assert_eq!(m.step(t0 + VERDICT_WINDOW, &home(), &mut e), [VERDICT_REGISTERED]);
}

#[test]
fn the_sign_in_page_is_reported_and_nothing_is_published_until_home() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, true);
    let signed_out = Inputs { landings: 2, ..Inputs::default() };
    assert_eq!(m.step(t0, &signed_out, &mut e), [SIGN_IN_PAGE, SIGN_IN_PAGE]);
    assert!(m.step(t0 + Duration::from_secs(600), &signed_out, &mut e).is_empty());
    assert!(e.published.is_empty(), "no timeout while signed out");
    assert_eq!(m.step(t0, &Inputs { landings: 2, ..home() }, &mut e).len(), 1);
    assert_eq!(e.published.len(), 1);
}

#[test]
fn the_no_publish_control_holds_the_link_and_publishes_nothing() {
    let t0 = Instant::now();
    let mut e = Fake::default();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, false);
    m.no_publish = true;
    assert_eq!(
        m.step(t0, &home(), &mut e),
        ["[deeplink] (home page interactive) not publishing (INTOXICATED_DEEPLINK_NO_PUBLISH)"]
    );
    assert_eq!(m.step(t0 + VERDICT_WINDOW, &home(), &mut e), [VERDICT_NOTHING]);
    assert!(e.published.is_empty());
}

#[test]
fn a_missing_or_failing_publish_is_reported_and_counted() {
    struct Missing;
    impl Engine for Missing {
        fn publish(&mut self, _: &str) -> Option<Result<(), String>> {
            None
        }
        fn cold_start_to_game(&mut self) -> Option<Result<bool, String>> {
            None
        }
    }
    let t0 = Instant::now();
    let mut m = machine("roblox://experiences/start?placeId=1818", true, false);
    assert_eq!(m.step(t0, &home(), &mut Missing), ["[deeplink] MessageBus.publishRaw is not exported by this build"]);
    assert!(m.published);
    assert_eq!(m.step(t0 + VERDICT_WINDOW, &home(), &mut Missing), [VERDICT_NOTHING]);
}
