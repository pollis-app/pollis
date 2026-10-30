// Keeps the app from being suspended while it holds the core's cross-process state lock (#1599).
//
// iOS terminates a suspended app (0xdead10cc) that still holds a file lock in an App Group
// container, which is where the core's data dir and `state.lock` live once the Notification
// Service Extension shares them. On entering the background this asks for background time and
// tells the core to start no new hold and drain the open ones (pollis_app_will_suspend); on
// returning, it lets holds start again. Holds are per MLS step and normally last well under a
// second; one waiting on a slow network ends within the DS client's own timeouts.

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>

// From pollis-core/include/pollis_notify.h (not on this pod's header path).
extern "C" bool pollis_app_will_suspend(uint32_t timeout_ms);
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
  UIApplication *app = [UIApplication sharedApplication];
  __block UIBackgroundTaskIdentifier task = UIBackgroundTaskInvalid;
  task = [app beginBackgroundTaskWithName:@"pollis.state-lock"
                        expirationHandler:^{
                          [app endBackgroundTask:task];
                          task = UIBackgroundTaskInvalid;
                        }];
  dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
    pollis_app_will_suspend(kDrainTimeoutMs);
    dispatch_async(dispatch_get_main_queue(), ^{
      if (task != UIBackgroundTaskInvalid) {
        [app endBackgroundTask:task];
        task = UIBackgroundTaskInvalid;
      }
    });
  });
}

@end
