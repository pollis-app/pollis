// Stands in for pollis-core's share ABI in the macOS harness (#2100). The PIN is 1234 with the
// app's 10 attempts; the device key "good" unlocks, "stale" is refused. SHARE_STATE picks what
// open reports ("signed_out", "locked_out", "locked", "locked_bio"); SHARE_SEND_FAIL makes the
// next send fail without storing.
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "stub.h"

static int attempts = 10;
static bool unlocked = false;
static int closes = 0;
static int sends = 0;
static char last_share_id[64];
static int nuked = 0;

int stub_close_count(void) { return closes; }
int stub_send_count(void) { return sends; }
int stub_attempts(void) { return attempts; }
const char *stub_last_share_id(void) { return last_share_id; }
void stub_reset(void) {
  attempts = 10;
  unlocked = false;
  closes = 0;
  sends = 0;
  nuked = 0;
  last_share_id[0] = 0;
}

void pollis_string_free(char *s) { free(s); }
void pollis_set_keychain_access_group(const char *group) { (void)group; }

char *pollis_share_open(const char *data_dir) {
  (void)data_dir;
  unlocked = false;
  const char *state = getenv("SHARE_STATE");
  if (state == NULL || strcmp(state, "signed_out") == 0 || nuked) {
    return strdup(nuked ? "{\"ok\":true,\"state\":\"signed_out\"}" : "{\"ok\":true,\"state\":\"signed_out\"}");
  }
  if (strcmp(state, "locked_out") == 0) {
    return strdup("{\"ok\":true,\"state\":\"locked_out\"}");
  }
  char *out = malloc(160);
  snprintf(out, 160, "{\"ok\":true,\"state\":\"locked\",\"biometric\":%s,\"attempts_remaining\":%d,\"user_id\":\"u1\"}",
           strcmp(state, "locked_bio") == 0 ? "true" : "false", attempts);
  return out;
}

char *pollis_share_unlock_pin(const char *pin) {
  if (nuked || attempts <= 0) {
    return strdup("{\"ok\":true,\"result\":\"locked_out\"}");
  }
  if (pin != NULL && strcmp(pin, "1234") == 0) {
    attempts = 10;
    unlocked = true;
    return strdup("{\"ok\":true,\"result\":\"unlocked\"}");
  }
  attempts -= 1;
  if (attempts <= 0) {
    nuked = 1;
    return strdup("{\"ok\":true,\"result\":\"locked_out\"}");
  }
  char *out = malloc(96);
  snprintf(out, 96, "{\"ok\":true,\"result\":\"incorrect\",\"attempts_remaining\":%d}", attempts);
  return out;
}

char *pollis_share_unlock_device_key(const char *key) {
  if (key != NULL && strcmp(key, "good") == 0) {
    unlocked = true;
    return strdup("{\"ok\":true,\"result\":\"unlocked\"}");
  }
  return strdup("{\"ok\":true,\"result\":\"biometric_failed\"}");
}

char *pollis_share_targets(const char *query, uint32_t limit) {
  (void)limit;
  if (!unlocked) {
    return strdup("{\"ok\":false,\"reason\":\"locked\"}");
  }
  if (query != NULL && strcmp(query, "zzz") == 0) {
    return strdup("{\"ok\":true,\"targets\":[]}");
  }
  if (query != NULL && strcmp(query, "net") == 0) {
    return strdup("{\"ok\":false,\"reason\":\"network\"}");
  }
  return strdup(
      "{\"ok\":true,\"targets\":["
      "{\"conversation_id\":\"vault\",\"kind\":\"vault\",\"title\":\"Vault\",\"subtitle\":null,\"avatar_user_id\":null,\"last_activity_at\":null,\"group_id\":null},"
      "{\"conversation_id\":\"d1\",\"kind\":\"dm\",\"title\":\"Ana\",\"subtitle\":\"@ana\",\"avatar_user_id\":\"a\",\"last_activity_at\":null,\"group_id\":null},"
      "{\"conversation_id\":\"c1\",\"kind\":\"channel\",\"title\":\"general\",\"subtitle\":\"Book club\",\"avatar_user_id\":null,\"last_activity_at\":null,\"group_id\":\"g1\"},"
      "{\"conversation_id\":\"d2\",\"kind\":\"dm\",\"title\":\"Bo\",\"subtitle\":\"@bo\",\"avatar_user_id\":\"b\",\"last_activity_at\":null,\"group_id\":null},"
      "{\"conversation_id\":\"d3\",\"kind\":\"dm\",\"title\":\"Cy\",\"subtitle\":\"@cy\",\"avatar_user_id\":\"c\",\"last_activity_at\":null,\"group_id\":null},"
      "{\"conversation_id\":\"d4\",\"kind\":\"dm\",\"title\":\"Di\",\"subtitle\":\"@di\",\"avatar_user_id\":\"d\",\"last_activity_at\":null,\"group_id\":null}"
      "]}");
}

// Very small JSON picking: the targets array's ids, in order.
static int target_ids(const char *json, char ids[8][32]) {
  const char *p = strstr(json, "\"targets\":[");
  int n = 0;
  if (p == NULL) {
    return 0;
  }
  p += strlen("\"targets\":[");
  while (*p && *p != ']' && n < 8) {
    if (*p == '"') {
      const char *end = strchr(p + 1, '"');
      size_t len = (size_t)(end - p - 1);
      memcpy(ids[n], p + 1, len < 31 ? len : 31);
      ids[n][len < 31 ? len : 31] = 0;
      n++;
      p = end + 1;
    } else {
      p++;
    }
  }
  return n;
}

char *pollis_share_send(const char *request_json, pollis_share_progress_fn progress, void *ctx) {
  sends += 1;
  const char *sid = strstr(request_json, "\"shareId\":\"");
  if (sid != NULL) {
    sid += strlen("\"shareId\":\"");
    size_t len = strcspn(sid, "\"");
    memcpy(last_share_id, sid, len < 63 ? len : 63);
    last_share_id[len < 63 ? len : 63] = 0;
  }
  if (getenv("SHARE_SEND_FAIL") != NULL) {
    return strdup("{\"ok\":false,\"reason\":\"network\"}");
  }
  char ids[8][32];
  int n = target_ids(request_json, ids);
  if (progress) {
    progress(ctx, "{\"event\":\"stored\",\"queued\":[]}");
  }
  char *out = malloc(2048);
  strcpy(out, "{\"ok\":true,\"results\":[");
  bool all_sent = getenv("SHARE_ALL_SENT") != NULL;
  for (int i = 0; i < n; i++) {
    const char *status = (i == 0 || all_sent) ? "sent" : "queued";
    char event[256];
    snprintf(event, sizeof event,
             "{\"event\":\"target\",\"conversation_id\":\"%s\",\"message_id\":\"m%d\",\"status\":\"%s\"}", ids[i], i, status);
    if (progress) {
      progress(ctx, event);
    }
    char row[200];
    snprintf(row, sizeof row, "%s{\"conversation_id\":\"%s\",\"message_id\":\"m%d\",\"status\":\"%s\"}",
             i ? "," : "", ids[i], i, status);
    strcat(out, row);
  }
  strcat(out, "]}");
  return out;
}

bool pollis_share_suspend(uint32_t timeout_ms) {
  (void)timeout_ms;
  return true;
}
void pollis_share_resume(void) {}
void pollis_share_close(void) {
  closes += 1;
  unlocked = false;
}
