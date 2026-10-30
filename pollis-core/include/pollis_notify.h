// Headless notification fetch (#1599). Plain C ABI exported by pollis-core
// (iOS xcframework static lib, Android .so).
//
// All strings UTF-8, NUL-terminated. Returned strings are owned by Rust; free
// with pollis_string_free. Never panics across the boundary; never returns
// NULL; errors are JSON:
//   {"ok":true,"mode":"full|name|none","items":[{"conversation_id":"...",
//     "message_id":"...","sender_name":"...","conversation_title":"..."|null,
//     "is_dm":true,"body":"...",
//     "body_key":"attachment|image|video|voice|sensitive"|null,
//     "muted":false}]}
//   {"ok":false,"reason":"not_logged_in|lock_busy|timeout|network|decrypt|internal"}
#ifndef POLLIS_NOTIFY_H
#define POLLIS_NOTIFY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

char *pollis_notify_fetch(const char *data_dir, const char *handle, uint32_t budget_ms);
void pollis_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif
