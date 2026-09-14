//! #1142: the OTP guess budget lives in memory, and the container sleeps.
//!
//! `OtpStore` is an in-process `HashMap` (`otp.rs`), so the failed-guess counter
//! and the mailbox lockout are lost whenever the container stops. That is not a
//! rare redeploy event: `PollisDelivery.sleepAfter` scales the container to zero
//! after a fixed idle period, so in a quiet hour the state is dropped routinely.
//!
//! **Why that is safe today, and the invariant it rests on.** To trigger the
//! reset an attacker has to send nothing for `sleepAfter` — their own requests
//! keep the container warm. As long as `sleepAfter >= ttl_secs`, that same
//! silence expires the code they were guessing, so they return to a fresh
//! counter AND a dead code. The reset cannot outlive the secret it protects, and
//! the guess budget stays bounded by the code lifetime rather than by the
//! lockout.
//!
//! Invert the inequality and the reasoning inverts with it: the counter would
//! reset while the code is still live, handing an attacker repeated fresh
//! `max_attempts` bursts against ONE code for the price of pausing between them.
//!
//! The two values sit in different languages in different files — `sleepAfter`
//! in `worker/index.ts`, whose own comment frames it purely as a cost lever and
//! so invites lowering it, and `ttl_secs` in Rust, overridable by
//! `OTP_TTL_SECS`. Nothing else connects them, which is exactly why this test
//! exists.

use pollis_delivery::otp::OtpConfig;

/// Parse the `sleepAfter` literal out of the container class.
///
/// Deliberately a source read rather than a constant copied into Rust: a copy
/// would be the thing that drifts.
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

/// The subset of `@cloudflare/containers`' `parseTimeExpression` that the config
/// may use: `<n>[smh]`, or a bare seconds count.
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

/// The container must not forget a mailbox's guess budget while the code that
/// budget protects is still valid.
///
/// Compares against the compiled-in default. `OTP_TTL_SECS` can raise `ttl_secs`
/// at runtime, which this test cannot see — that path is called out in
/// `OtpConfig::from_env`, because a deploy-time env change is the other way the
/// inequality can invert.
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

/// The time-expression parser above must actually parse, or the assertion is
/// reading a number it invented.
#[test]
fn the_sleep_after_parser_handles_the_forms_the_config_may_use() {
    assert_eq!(parse_time_expression("10m"), Some(600));
    assert_eq!(parse_time_expression("24h"), Some(86_400));
    assert_eq!(parse_time_expression("90s"), Some(90));
    assert_eq!(parse_time_expression("600"), Some(600));
    assert_eq!(parse_time_expression("soon"), None);

    // And it read a real value from the real file, not a default.
    assert!(
        sleep_after_secs() > 0,
        "sleepAfter parsed as 0 — the source read is not finding the declaration"
    );
}
