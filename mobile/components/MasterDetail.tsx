import { useEffect, type ComponentType, type ReactNode } from "react";
import { BackHandler, View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import { semantic, type as ty, layout } from "../theme/tokens";
import {
  PaneContext,
  PaneEntryContext,
  type PanePath,
  type PaneStack,
} from "./pane/paneContext";
import ThreadScreen from "../app/chat/thread";
import ConversationInfo from "../app/conversation/info";
import PinnedMessages from "../app/conversation/pins";
import DMInfo from "../app/dm/info";
import DMSafetyNumber from "../app/dm/safety";
import UserProfile from "../app/user/[id]";
import GroupSettings from "../app/group/settings";
import GroupMembers from "../app/group/members";
import InviteToGroup from "../app/group/invite";
import GroupInviteLinks from "../app/group/invite-links";
import GroupEmoji from "../app/group/emoji";
import JoinRequests from "../app/group/requests";

// Two-pane master-detail primitives for the regular (iPad) layout (#622);
// the compact tree never mounts these.

// The same component each route file renders on phones; `useRouteParams` and
// `useNav` give it the pane entry's params and pane pushes.
const PANE_SCREENS: Record<PanePath, ComponentType> = {
  "/chat/thread": ThreadScreen,
  "/conversation/info": ConversationInfo,
  "/conversation/pins": PinnedMessages,
  "/dm/info": DMInfo,
  "/dm/safety": DMSafetyNumber,
  "/user/[id]": UserProfile,
  "/group/settings": GroupSettings,
  "/group/members": GroupMembers,
  "/group/invite": InviteToGroup,
  "/group/invite-links": GroupInviteLinks,
  "/group/emoji": GroupEmoji,
  "/group/requests": JoinRequests,
};

export function DetailPlaceholder() {
  const { t } = useTranslation("mobile");
  return (
    <View
      style={{
        flex: 1,
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
      }}
    >
      <Text style={[ty.secondary, { color: semantic.muted, textAlign: "center" }]}>
        {t("ui.selectConversation")}
      </Text>
    </View>
  );
}

// Provides a tab's pane stack to the left column (group pages opened from the
// group panel push into the pane) and the right pane.
export function PaneProvider({
  pane,
  children,
}: {
  pane: PaneStack;
  children: ReactNode;
}) {
  return <PaneContext.Provider value={pane}>{children}</PaneContext.Provider>;
}

// Every pushed page stays mounted while covered, so going back returns to the
// conversation with its draft and scroll position intact.
export function DetailPane({
  pane,
  root,
}: {
  pane: PaneStack;
  root: ReactNode;
}) {
  const depth = pane.entries.length;
  const back = pane.api.back;

  // Android tablets: the hardware back pops the pane before it leaves the tab.
  useEffect(() => {
    if (depth === 0) {
      return;
    }
    const sub = BackHandler.addEventListener("hardwareBackPress", () => {
      back();
      return true;
    });
    return () => sub.remove();
  }, [depth, back]);

  return (
    <View style={{ flex: 1 }}>
      <View style={{ flex: 1, display: depth === 0 ? "flex" : "none" }}>{root}</View>
      {pane.entries.map((entry, i) => {
        const Page = PANE_SCREENS[entry.pathname];
        return (
          <View
            key={entry.key}
            style={{ flex: 1, display: i === depth - 1 ? "flex" : "none" }}
          >
            <PaneEntryContext.Provider value={entry}>
              <Page />
            </PaneEntryContext.Provider>
          </View>
        );
      })}
    </View>
  );
}

export function TwoPane({
  list,
  detail,
}: {
  list: ReactNode;
  detail: ReactNode;
}) {
  return (
    <View style={{ flexDirection: "row", flex: 1 }}>
      <View style={{ width: layout.listPaneWidth }}>{list}</View>
      <View style={{ width: 1, backgroundColor: semantic.hairSoft }} />
      <View style={{ flex: 1 }}>{detail}</View>
    </View>
  );
}

// A tab's empty state on regular width: no two-pane, one centred column at
// `layout.screenMaxWidth`.
export function CenteredColumn({ children }: { children: ReactNode }) {
  return (
    <View
      style={{
        flex: 1,
        width: "100%",
        maxWidth: layout.screenMaxWidth,
        alignSelf: "center",
      }}
    >
      {children}
    </View>
  );
}
