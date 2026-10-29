//! #1142: the OTP guess budget lives in memory, and the container sleeps.
//!
//! Safe only while `sleepAfter >= ttl_secs`: the silence that resets the counter also expires the code.

use pollis_delivery::otp::OtpConfig;

/// Parse `sleepAfter` from source: a constant copied into Rust would drift.
fn sleep_after_secs() -> u64 {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("worker/index.ts"),
    )
    .expect("read worker/index.ts");

    let line = src
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("sleepAfter"))
        .expect(
            "worker/index.ts no longer declares `sleepAfter` — if the container \
             lifetime moved, this invariant needs re-deriving, not deleting",
        );

    let raw = line
        .split('"')
        .nth(1)
        .expect("sleepAfter must be a quoted time expression");

    parse_time_expression(raw)
        .unwrap_or_else(|| panic!("could not parse sleepAfter value {raw:?}"))
}

/// The `parseTimeExpression` subset the config may use: `<n>[smh]` or bare seconds.
fn parse_time_expression(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (digits, mult) = match raw.chars().last()? {
        's' => (&raw[..raw.len() - 1], 1),
        'm' => (&raw[..raw.len() - 1], 60),
        'h' => (&raw[..raw.len() - 1], 3600),
        '0'..='9' => (raw, 1),
        _ => return None,
    };
    digits.parse::<u64>().ok().map(|n| n * mult)
}

/// `OTP_TTL_SECS` can raise `ttl_secs` at runtime, which this test cannot see.
#[test]
fn the_container_sleeps_no_sooner_than_an_otp_expires() {
    let sleep_after = sleep_after_secs();
    let ttl = OtpConfig::default().ttl_secs;

    assert!(
        sleep_after >= ttl,
        "sleepAfter is {sleep_after}s but an OTP lives {ttl}s. The failed-guess \
         counter and mailbox lockout are in-memory (#1142), so the container \
         going to sleep clears them. That is only safe while the silence needed \
         to trigger it also expires the code being attacked. With sleepAfter \
         BELOW the code lifetime, an attacker gets a fresh {}-guess budget \
         against one live code every {sleep_after}s just by pausing. Raise \
         sleepAfter back to at least {ttl}s, or make the lockout durable.",
        OtpConfig::default().max_attempts
    );
}

#[test]
fn the_sleep_after_parser_handles_the_forms_the_config_may_use() {
    assert_eq!(parse_time_expression("10m"), Some(600));
    assert_eq!(parse_time_expression("24h"), Some(86_400));
    assert_eq!(parse_time_expression("90s"), Some(90));
    assert_eq!(parse_time_expression("600"), Some(600));
    assert_eq!(parse_time_expression("soon"), None);

    assert!(
        sleep_after_secs() > 0,
        "sleepAfter parsed as 0 — the source read is not finding the declaration"
    );
}
