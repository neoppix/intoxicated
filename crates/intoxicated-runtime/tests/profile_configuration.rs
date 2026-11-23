//! A launch reads its flag overrides from the profile it was given.
//!
//! An integration test, so it has a process of its own: the profile is fixed
//! through `profile::set_active`, a `OnceLock`. That is a per-process fact, so
//! this needs a process that has not already decided it, which is what an
//! integration test is.

#[test]
fn a_launch_reads_its_flags_from_the_profile_it_was_given() {
    let root = std::env::temp_dir().join(format!("intoxicated-profile-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let profile = root.join("profiles/alt");
    std::fs::create_dir_all(&profile).unwrap();
    // A file named outright would skip the profile.
    std::env::remove_var("INTOXICATED_FLAGS");

    // The profile arrives as one decision, the way `--profile` hands it over,
    // and everything below is resolved from it.
    intoxicated_runtime::profile::set_active(profile.clone()).expect("the first profile is accepted");

    let document = r#"{"FIntTaskSchedulerAutoThreadLimit":"8"}"#;
    std::fs::write(profile.join("flags.json"), document).unwrap();

    let resolved = intoxicated_runtime::flag_layers::resolve(intoxicated_runtime::flag_layers::collect());
    assert_eq!(
        resolved.get("FIntTaskSchedulerAutoThreadLimit").map(|r| r.value.as_str()),
        Some("8"),
        "{resolved:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
