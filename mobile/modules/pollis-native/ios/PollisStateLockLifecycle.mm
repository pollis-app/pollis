// Keeps the app from being suspended while it holds a lock in the App Group container (#1599).
//
// iOS terminates a suspended app (0xdead10cc) that still holds a file lock in an App Group
// container. Once the Notification Service Extension shares the core's data dir, that is
// where `state.lock` and the SQLCipher database (whose WAL keeps fcntl locks on -shm) live.
// On entering the background this asks for background time and lets the core stop new
// `state.lock` holds, drain the open ones, checkpoint and close the database
// (pollis_app_will_suspend). On returning it reopens them (pollis_app_did_resume). The ticket,
// taken synchronously, makes a drain that only starts after a quick return a no-op, so the app
// can never stay gated.

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>

// From pollis-core/include/pollis_notify.h (not on this pod's header path).
extern "C" uint64_t pollis_app_suspend_ticket(void);
extern "C" bool pollis_app_will_suspend(uint64_t ticket, uint32_t timeout_ms);
extern "C" void pollis_app_did_resume(void);

// Below the ~30 s iOS grants a background task, leaving room to end it cleanly.
static const uint32_t kDrainTimeoutMs = 25000;

@interface PollisStateLockLifecycle : NSObject
@end

@implementation PollisStateLockLifecycle

+ (void)load
{
  NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
  [center addObserverForName:UIApplicationDidEnterBackgroundNotification
                      object:nil
                       queue:nil
                  usingBlock:^(NSNotification *note) {
                    [PollisStateLockLifecycle drainForSuspension];
                  }];
  [center addObserverForName:UIApplicationWillEnterForegroundNotification
                      object:nil
                       queue:nil
                  usingBlock:^(NSNotification *note) {
                    pollis_app_did_resume();
                  }];
}

+ (void)drainForSuspension
{
  // Synchronously, before anything is dispatched: a later resume voids this ticket.
  uint64_t ticket = pollis_app_suspend_ticket();
  UIApplication *app = [UIApplication sharedApplication];
  __block UIBackgroundTaskIdentifier task = UIBackgroundTaskInvalid;
  task = [app beginBackgroundTaskWithName:@"pollis.state-lock"
                        expirationHandler:^{
                          [app endBackgroundTask:task];
                          task = UIBackgroundTaskInvalid;
                        }];
  dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
    pollis_app_will_suspend(ticket, kDrainTimeoutMs);
    dispatch_async(dispatch_get_main_queue(), ^{
      if (task != UIBackgroundTaskInvalid) {
        [app endBackgroundTask:task];
        task = UIBackgroundTaskInvalid;
      }
    });
  });
}

@end
