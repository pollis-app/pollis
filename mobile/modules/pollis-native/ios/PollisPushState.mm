// What the Notification Service Extension needs from the running app (#1599): the live accent
// and background, so its letter avatars match the app's. Stored in the App Group's UserDefaults
// (suite = Info.plist `PollisAppGroup`), read by plugins/notification-service/LetterAvatar.swift.
// Same JS surface as Android's PollisPushState; setOpenConversation is a no-op here, because an
// iOS extension cannot drop a push.

#import <Foundation/Foundation.h>
#import <React/RCTBridgeModule.h>

static NSString *const kAccentKey = @"pollis.theme.accent";
static NSString *const kBackgroundKey = @"pollis.theme.bg";

static BOOL PollisIsHexColour(NSString *hex)
{
  if (![hex isKindOfClass:[NSString class]]) {
    return NO;
  }
  NSString *h = [hex hasPrefix:@"#"] ? [hex substringFromIndex:1] : hex;
  if (h.length != 6) {
    return NO;
  }
  NSCharacterSet *notHex =
      [[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdefABCDEF"] invertedSet];
  return [h rangeOfCharacterFromSet:notHex].location == NSNotFound;
}

@interface PollisPushState : NSObject <RCTBridgeModule>
@end

@implementation PollisPushState

RCT_EXPORT_MODULE()

+ (BOOL)requiresMainQueueSetup
{
  return NO;
}

RCT_EXPORT_METHOD(setThemeColors : (NSString *)accentHex bg : (NSString *)bgHex)
{
  if (!PollisIsHexColour(accentHex) || !PollisIsHexColour(bgHex)) {
    return;
  }
  NSString *group = [[NSBundle mainBundle] objectForInfoDictionaryKey:@"PollisAppGroup"];
  if (![group isKindOfClass:[NSString class]] || group.length == 0) {
    return;
  }
  NSUserDefaults *defaults = [[NSUserDefaults alloc] initWithSuiteName:group];
  [defaults setObject:accentHex forKey:kAccentKey];
  [defaults setObject:bgHex forKey:kBackgroundKey];
}

RCT_EXPORT_METHOD(setOpenConversation : (NSString *)conversationId)
{
}

@end
