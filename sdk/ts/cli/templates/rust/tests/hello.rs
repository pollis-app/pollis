use pollis_app::testing::{AuthorInput, JobOptions, MessageInput, TestApp};

// Each test runs the built App in the same sandbox the bot runtime uses, offline.

#[test]
fn hello_greets_by_name() {
    let result = TestApp::new().command("hello", "Sam");
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.replies, ["Hello, Sam!"]);
}

#[test]
fn hello_with_no_name_greets_the_sender() {
    let app = TestApp::new();
    let sender = MessageInput::default().author(AuthorInput::named("Ann"));
    let result = app.with(JobOptions::new()).command_from("hello", "", sender);
    assert_eq!(result.replies, ["Hello, Ann!"]);
}

#[test]
fn a_command_the_app_did_not_declare_never_reaches_it() {
    let result = TestApp::new().command("weather", "");
    assert!(result.actions.is_empty());
}
