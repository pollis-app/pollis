import { reaction } from "mobx";

import { appStore } from "../stores/appStore";

export { detachChannel } from "./detachChannel";

/** Run `teardown` whenever the signed-in user goes away (logout, account switch to none). */
export function onSignOut(teardown: () => void): void {
  reaction(
    () => appStore.currentUser,
    (user) => {
      if (!user) {
        teardown();
      }
    },
  );
}
