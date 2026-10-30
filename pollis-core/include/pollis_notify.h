// Rich push notifications decrypted on the device (#1599). Plain C ABI exported by pollis-core:
// iOS links it from the xcframework's libpollis_core.a (the app AND the Notification Service
// Extension each link it); Android gets it inside libpollis-native.so, where a JNI wrapper in
// cpp-adapter.cpp must reference pollis_notify_fetch so the linker keeps it.
//
// All strings UTF-8, NUL-terminated. Returned strings are owned by Rust; free them with
// pollis_string_free. No function returns NULL; errors are JSON. Safe to call from any thread,
// before or without init_pollis. (Release builds use panic=abort: a Rust panic ends the process.)

#ifndef POLLIS_NOTIFY_H
#define POLLIS_NOTIFY_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Fetch, decrypt and persist pending envelopes for the push handle `handle` (the `h` of the
// push payload; NULL = no fetch, only what was stored in the last two minutes), exactly as the
// app would, and return previews. `data_dir` is the core's data dir: on iOS the App Group
// container + "/pollis"; on Android the same path the app passes to init_pollis as data_dir.
// `budget_ms` bounds the whole call (0 = 20000); at most 40% of it is spent waiting for the app
// to release state.lock.
//
//   {"ok":true,"mode":"full|name|none","items":[{"conversation_id":"...",
//     "message_id":"...","sender_name":"...","conversation_title":"..."|null,
//     "is_dm":true,"body":"...",
//     "body_key":"attachment|image|video|voice|sensitive"|null,
//     "muted":false,"mention":false}]}
//   {"ok":false,"reason":"not_logged_in|lock_busy|timeout|network|decrypt|internal"}
//
// items are the messages THIS call stored, oldest first, so a message is returned by at most
// one call. If the call stored nothing for the handle's conversation (the app already had it),
// the newest message there from the last 10 minutes is returned once instead; hosts may also
// dedupe by message_id. With mode "name" every body is "" and body_key null; mode "none"
// never fetches or decrypts anything and returns no items. "muted" means the channel is muted
// and the message does not @mention this user (hosts must not show it); "mention" is true when
// it does. mode "none" with no items means the user chose Nothing, or has not unlocked the app
// since installing this build: show the generic text.
char *pollis_notify_fetch(const char *data_dir, const char *handle, uint32_t budget_ms);

// Free a string returned above. NULL is a no-op.
void pollis_string_free(char *s);

// App only, before init_pollis (e.g. +load): relocate the data dir to `dir` (iOS: the App Group
// container + "/pollis") without changing the keystore namespace. init_pollis then moves the
// old Library/Application Support/pollis there once. NULL or "" is ignored.
void pollis_set_data_dir_override(const char *dir);

// iOS, app AND extension, before anything touches the keystore: the keychain access group
// shared by both, e.g. "<TEAMID>.com.pollis.shared". Without it the core reads the Info.plist
// key PollisKeychainAccessGroup. The app migrates its keystore key into this group; the
// extension never creates one. No-op on other platforms.
void pollis_set_keychain_access_group(const char *group);

// App only (iOS 0xdead10cc). In applicationDidEnterBackground take a ticket SYNCHRONOUSLY,
// then, inside a background-task assertion and off the main thread (it blocks up to
// timeout_ms), call pollis_app_will_suspend(ticket, ...): no new state.lock hold starts, the
// open ones are drained, and the database connection is checkpointed and closed. Returns true
// when nothing is held. A ticket older than the last pollis_app_did_resume does nothing.
uint64_t pollis_app_suspend_ticket(void);
bool pollis_app_will_suspend(uint64_t ticket, uint32_t timeout_ms);

// App only: back in the foreground. Opens the gate and reopens the database.
void pollis_app_did_resume(void);

#ifdef __cplusplus
}
#endif

#endif
