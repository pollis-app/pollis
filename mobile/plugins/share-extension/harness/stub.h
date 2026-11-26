// The harness's view of the stub core (#2100): the real share ABI plus counters a case reads.
#import "pollis_notify.h"
#import "pollis_share.h"
int stub_close_count(void);
int stub_send_count(void);
int stub_attempts(void);
const char *stub_last_share_id(void);
void stub_reset(void);
