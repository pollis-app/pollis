// A video's still frame from the platform decoder (#1837): AVAssetImageGenerator, oriented by the
// track's transform, scaled to fit maxEdge, returned as base64 JPEG. In memory only: no file is
// written. JS side: lib/media/videoFrame.ts (Android twin: PollisVideoFrameModule.kt).

#import <AVFoundation/AVFoundation.h>
#import <Foundation/Foundation.h>
#import <React/RCTBridgeModule.h>
#import <UIKit/UIKit.h>

@interface PollisVideoFrame : NSObject <RCTBridgeModule>
@end

@implementation PollisVideoFrame

RCT_EXPORT_MODULE()

+ (BOOL)requiresMainQueueSetup
{
  return NO;
}

- (dispatch_queue_t)methodQueue
{
  return dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0);
}

RCT_EXPORT_METHOD(frame : (NSString *)uri maxEdge : (double)maxEdge resolve : (RCTPromiseResolveBlock)
                      resolve reject : (RCTPromiseRejectBlock)reject)
{
  NSURL *url = [uri containsString:@"://"] ? [NSURL URLWithString:uri] : [NSURL fileURLWithPath:uri];
  if (url == nil) {
    reject(@"E_VIDEO_FRAME", @"not a video uri", nil);
    return;
  }
  AVURLAsset *asset = [AVURLAsset URLAssetWithURL:url options:nil];
  AVAssetImageGenerator *generator = [AVAssetImageGenerator assetImageGeneratorWithAsset:asset];
  generator.appliesPreferredTrackTransform = YES;
  CGFloat edge = maxEdge > 0 ? maxEdge : 640;
  generator.maximumSize = CGSizeMake(edge, edge);
  generator.requestedTimeToleranceBefore = kCMTimePositiveInfinity;
  generator.requestedTimeToleranceAfter = kCMTimePositiveInfinity;

  // Half a second in (past a fade from black), or a tenth of a shorter clip; desktop does the same.
  Float64 duration = CMTimeGetSeconds(asset.duration);
  Float64 at = isfinite(duration) && duration > 0 ? MIN(0.5, duration * 0.1) : 0;

  [generator generateCGImageAsynchronouslyForTime:CMTimeMakeWithSeconds(at, 600)
                                completionHandler:^(CGImageRef image, CMTime actual, NSError *error) {
                                  if (image == NULL) {
                                    reject(@"E_VIDEO_FRAME", error.localizedDescription ?: @"no frame", error);
                                    return;
                                  }
                                  NSData *jpeg = UIImageJPEGRepresentation([UIImage imageWithCGImage:image], 0.8);
                                  if (jpeg == nil) {
                                    reject(@"E_VIDEO_FRAME", @"jpeg encode failed", nil);
                                    return;
                                  }
                                  resolve([jpeg base64EncodedStringWithOptions:0]);
                                }];
}

@end
