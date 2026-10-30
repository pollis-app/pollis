// Points the Rust core at the App Group container before anything can initialise it (#1599).
//
// The Notification Service Extension decrypts pushes against the same data dir and keystore key,
// so both must live where both processes can reach them. The keys come from the app's Info.plist,
// written by plugins/withNotificationServiceExtension.js; a build without them (POLLIS_IOS_NSE=off,
// or a binary from before #1599) keeps the old `Library/Application Support/pollis`. On the first
// launch with the override, init_pollis moves the old directory into the container.

#import <Foundation/Foundation.h>

// From pollis-core/include/pollis_notify.h (not on this pod's header path).
extern "C" void pollis_set_data_dir_override(const char *dir);
extern "C" void pollis_set_keychain_access_group(const char *group);

@interface PollisAppGroup : NSObject
@end

@implementation PollisAppGroup

// +load runs before main, on the only thread there is, so nothing can race the override.
+ (void)load
{
  NSDictionary *info = [[NSBundle mainBundle] infoDictionary];
  NSString *keychainGroup = info[@"PollisKeychainAccessGroup"];
  if ([keychainGroup isKindOfClass:[NSString class]] && keychainGroup.length > 0) {
    pollis_set_keychain_access_group(keychainGroup.UTF8String);
  }
  NSString *group = info[@"PollisAppGroup"];
  if (![group isKindOfClass:[NSString class]] || group.length == 0) {
    return;
  }
  NSURL *container =
      [[NSFileManager defaultManager] containerURLForSecurityApplicationGroupIdentifier:group];
  if (container == nil) {
    // Not entitled (mis-signed build): stay on the old directory rather than lose it.
    return;
  }
  NSURL *dir = [container URLByAppendingPathComponent:@"pollis" isDirectory:YES];
  pollis_set_data_dir_override(dir.path.fileSystemRepresentation);
}

@end
