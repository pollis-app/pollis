// The one registry the tiles report to, wired to Rust's `set_video_wants`.

import { invoke } from "../bridge";
import { VideoWantsRegistry } from "./videoWantsRegistry";

export const videoWants = new VideoWantsRegistry((trackKey, wants) =>
  invoke("set_video_wants", { trackKey, wants }),
);
