// Stands in for pollis-core in the macOS harness: returns $NSE_FIXTURE as the fetch result.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

char *pollis_notify_fetch(const char *data_dir, const char *handle, uint32_t budget_ms) {
  (void)data_dir;
  (void)budget_ms;
  const char *fixture = getenv("NSE_FIXTURE");
  if (fixture == NULL) {
    return strdup("{\"ok\":false,\"reason\":\"internal\"}");
  }
  // Lets a case assert the handle reached the core.
  if (handle != NULL && strcmp(fixture, "ECHO_HANDLE") == 0) {
    size_t n = strlen(handle) + 64;
    char *out = malloc(n);
    snprintf(out, n, "{\"ok\":false,\"reason\":\"%s\"}", handle);
    return out;
  }
  return strdup(fixture);
}

void pollis_string_free(char *s) {
  free(s);
}
