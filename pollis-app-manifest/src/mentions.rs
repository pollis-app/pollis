//! The `@mention` rule (#843), in one place: pollis-core uses it to decide
//! whom a channel message wakes, the bot runtime to route a message to an
//! App's `on-mention` (#2004), and `pollis-app-dev` to route it the same way
//! in offline tests (#2006).

/// Characters that may appear INSIDE a username after the leading `@`. Kept
/// deliberately wide (the `users.username` column has no charset constraint)
/// but closed over the punctuation that normally ends a sentence, so
/// "@dana." and "@dana," yield the token `dana`.
fn is_mention_body_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Extract the `@`-prefixed mention tokens from `content`, lowercased and
/// stripped of the leading `@`.
///
/// Only a string-initial or whitespace-preceded `@` counts; `@all` is excluded.
///
/// MIRROR: `mentionTokens()` in `frontend/src/utils/mentions.ts`. The two must
/// agree or the composer will promise a ping the backend does not send.
pub fn mention_tokens(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' {
            i += 1;
            continue;
        }
        // Only a string-initial or whitespace-preceded `@` opens a mention.
        if i > 0 && !chars[i - 1].is_whitespace() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && is_mention_body_char(chars[j]) {
            j += 1;
        }
        let token: String = chars[i + 1..j]
            .iter()
            .collect::<String>()
            .trim_end_matches(['.', '-', '_'])
            .to_lowercase();
        if !token.is_empty() && token != "all" {
            tokens.push(token);
        }
        i = j.max(i + 1);
    }
    tokens
}

/// Whether `content` mentions `username` by `@username`, by the same rule
/// that decides who a channel message wakes (#843): a whitespace- or
/// string-initial `@`, case-insensitive, trailing punctuation ignored. `@all`
/// is not a mention of the bot (#2004: an App's `on-mention`).
pub fn mentions_username(content: &str, username: &str) -> bool {
    let want = username.to_lowercase();
    !want.is_empty() && mention_tokens(content).contains(&want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_is_mentioned_by_its_username_only() {
        assert!(mentions_username("hey @Triage_bot, look", "triage_bot"));
        assert!(mentions_username("@triage_bot", "triage_bot"));
        assert!(!mentions_username("@all please", "triage_bot"));
        assert!(!mentions_username("mail@triage_bot", "triage_bot"));
        assert!(!mentions_username("@triage_bots", "triage_bot"));
    }

    #[test]
    fn mention_tokens_ignores_bare_at_and_embedded_at() {
        assert_eq!(mention_tokens("@ hello"), Vec::<String>::new());
        assert_eq!(mention_tokens("user@example.com"), Vec::<String>::new());
        assert_eq!(mention_tokens("hi @dana"), vec!["dana".to_string()]);
    }
}
