// The outgoing screen-share and camera resolution (#1902): one synced preference each, read and set
// from both the Voice & Video settings and the in-call resolution panel. A change lands in the
// preference cache on the same frame (both surfaces re-render from it) and moves the running
// stream's ceiling at once, with no restart.

import { useCallback } from "react";

import { usePreferences } from "./queries/usePreferences";
import { screenShareSession } from "../screenshare/screenShareSession";
import { cameraSession } from "../camera/cameraSession";
import {
  CAMERA_RESOLUTION_DEFAULT,
  SCREEN_SHARE_RESOLUTION_DEFAULT,
} from "../voice/videoResolution";

export interface OutgoingResolution {
  shareMaxHeight: number;
  setShareMaxHeight: (maxHeight: number) => void;
  cameraMaxHeight: number;
  setCameraMaxHeight: (maxHeight: number) => void;
}

export function useOutgoingResolution(): OutgoingResolution {
  const { query, save } = usePreferences();
  const data = query.data;

  const setShareMaxHeight = useCallback(
    (maxHeight: number) => {
      save({ ...data, screen_share_max_height: maxHeight });
      void screenShareSession.setMaxHeight(maxHeight).catch((err) => {
        console.warn("[screenshare] set_screen_share_max_height failed:", err);
      });
    },
    [data, save],
  );

  const setCameraMaxHeight = useCallback(
    (maxHeight: number) => {
      save({ ...data, camera_max_height: maxHeight });
      void cameraSession.setMaxHeight(maxHeight).catch((err) => {
        console.warn("[camera] set_camera_max_height failed:", err);
      });
    },
    [data, save],
  );

  return {
    shareMaxHeight: data?.screen_share_max_height ?? SCREEN_SHARE_RESOLUTION_DEFAULT,
    setShareMaxHeight,
    cameraMaxHeight: data?.camera_max_height ?? CAMERA_RESOLUTION_DEFAULT,
    setCameraMaxHeight,
  };
}
