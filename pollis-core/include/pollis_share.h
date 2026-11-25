// Sharing into Pollis from another app's share sheet (#2100). Plain C ABI exported by
// pollis-core for the iOS share extension (PollisShare), which links libpollis_core.a from the
// app's xcframework the way the Notification Service Extension does.
//
// All strings UTF-8, NUL-terminated. Returned strings are owned by Rust; free them with
// pollis_string_free (pollis_notify.h). No function returns NULL; errors are JSON. Every call
// BLOCKS (Argon2id, SQLCipher, network): call off the main thread. One session per process.
//
// Lifecycle:
//   pollis_share_open(data_dir)            -> who is signed in, locked
//   pollis_share_unlock_pin / _device_key  -> the app's own PIN counter and lockout
//   pollis_share_targets(query, limit)     -> the picker's rows
//   pollis_share_send(request, cb, ctx)    -> stored for every target, then delivered
//   pollis_share_suspend / _resume         -> the host app left / returned to the foreground
//   pollis_share_close()                   -> always, before completing or cancelling
// A send the extension does not finish stays in the app's outbox and the app delivers it.

#ifndef POLLIS_SHARE_H
#define POLLIS_SHARE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Start a locked session for whoever is signed in. `data_dir` is the App Group container +
// "/pollis". Call pollis_set_keychain_access_group first.
//   {"ok":true,"state":"signed_out"}      nobody signed in: show "Sign in to Pollis first"
//   {"ok":true,"state":"locked_out"}      the PIN was locked out: only the app can recover
//   {"ok":true,"state":"locked","biometric":true|false,"attempts_remaining":10,"user_id":"..."}
//   (user_id names the biometric keychain item: service "pollis.deviceUnlock:auth", account
//   "duk.<user_id>", access group "<team>.com.pollis.app")
char *pollis_share_open(const char *data_dir);

// Unlock with the PIN (4 digits), counted against the app's own attempts.
//   {"ok":true,"result":"unlocked"}
//   {"ok":true,"result":"incorrect","attempts_remaining":7}
//   {"ok":true,"result":"locked_out"}     the keys are gone
//   {"ok":true,"result":"open_app"}       the database would not open here (finish an update)
//   {"ok":true,"result":"no_session"}
char *pollis_share_unlock_pin(const char *pin);

// Unlock with the device unlock key the host read (base64) from the app's biometric-bound
// keychain item. As above plus {"ok":true,"result":"biometric_failed"}; never spends an attempt.
char *pollis_share_unlock_device_key(const char *device_key);

// The conversations this user may share to, best first (`query` NULL or "" for all; `limit` 0
// for the default). Never a DM request, a blocked DM or a voice channel.
//   {"ok":true,"targets":[{"conversation_id":"...","kind":"dm|channel","title":"...",
//     "subtitle":"..."|null,"avatar_user_id":"..."|null,"last_activity_at":"..."|null,
//     "group_id":"..."|null}]}
//   {"ok":false,"reason":"locked|network|internal"}
char *pollis_share_targets(const char *query, uint32_t limit);

// Progress of a send. `event` is JSON valid only during the call:
//   {"event":"stored","queued":[{"conversation_id":"...","message_id":"..."}]}
//   {"event":"target","conversation_id":"...","message_id":"...","status":"sent|queued|rejected"}
// Called from a core worker thread.
typedef void (*pollis_share_progress_fn)(void *ctx, const char *event);

// Store and deliver one share. `request_json`:
//   {"shareId":"<ULID, minted once, reused on retry>","targets":["<conversation id>", ...1-5],
//    "text":"...","attachments":[{"path":"/abs/file","filename":"a.jpg","contentType":"image/jpeg",
//      "width":4032,"height":3024,"blurhash":"...","poster":"<base64 jpeg>"|null}, ...0-10]}
// Files are read from disk a chunk at a time and may be deleted once "stored" has fired.
//   {"ok":true,"results":[{"conversation_id","message_id","status":"sent|queued|rejected"}]}
//   {"ok":false,"reason":"invalid|locked|network|internal"}   nothing new was stored
char *pollis_share_send(const char *request_json, pollis_share_progress_fn progress, void *ctx);

// The host app is leaving the foreground (NSExtensionHostWillResignActive), inside an expiring
// activity: let sends in flight finish for up to 3/4 of `timeout_ms`, then release state.lock and
// close the database, so the suspended extension holds nothing in the App Group (0xdead10cc).
bool pollis_share_suspend(uint32_t timeout_ms);

// The host app is active again: reopen what pollis_share_suspend closed.
void pollis_share_resume(void);

// End the session: release the lock, close the database, forget the keys.
void pollis_share_close(void);

#ifdef __cplusplus
}
#endif

#endif
