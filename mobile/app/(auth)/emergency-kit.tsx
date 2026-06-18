import { useState } from "react";
import { View, Text, Pressable, ScrollView } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  Card,
  Button,
  BottomAction,
} from "../../components/ui";
import { PollisMark } from "../../components/PollisMark";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts, r } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

/**
 * Emergency Kit — shown once, right after a brand-new account's PIN is set.
 * The recovery key emitted by `verify_otp` lives in the MobX store
 * (`pendingSecretKey`) for one screen-jump only. The user must explicitly
 * acknowledge they've saved it before we drop it from memory.
 *
 * After ACK we route to /(auth)/initializing — exactly the same path a
 * returning user takes — so the rest of the launch sequence stays the
 * same regardless of whether this was a new account or not.
 */
function EmergencyKit() {
  const router = useRouter();
  const pendingSecretKey = appStore.pendingSecretKey;
  const setPendingSecretKey = appStore.setPendingSecretKey;
  const [acknowledged, setAcknowledged] = useState(false);

  // Defensive: if someone deep-links here without a stashed key, just
  // continue to initializing — nothing to display.
  if (!pendingSecretKey) {
    router.replace("/(auth)/initializing");
    return null;
  }

  const onContinue = () => {
    setPendingSecretKey(null);
    router.replace("/(auth)/initializing");
  };

  return (
    <Screen>
      <Crumb
        segs={[{ label: "AUTH" }, { label: "Emergency kit", leaf: true }]}
      />
      <Body>
        <ScrollView contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 18, gap: 18, paddingBottom: 24 }}>
          <PollisMark />
          <View style={{ gap: 8 }}>
            <Text style={[ty.h1, { color: semantic.ink }]}>Save your recovery key</Text>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                lineHeight: 19,
                color: semantic.mute,
              }}
            >
              This key is the only way to add Pollis to a new device or
              recover after losing this one. We don't store it anywhere
              you can read it later — write it down or save it to a
              password manager now.
            </Text>
          </View>

          <Card style={{ borderColor: semantic.accent }}>
            <Text
              selectable
              style={{
                fontFamily: fonts.mono400,
                fontSize: 14,
                lineHeight: 22,
                color: semantic.accent,
                letterSpacing: 0.4,
              }}
            >
              {pendingSecretKey}
            </Text>
          </Card>

          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
            }}
          >
            <Icon.shield color={semantic.mute} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 11,
                color: semantic.mute,
                flex: 1,
                lineHeight: 16,
              }}
            >
              Anyone with this key can sign in as you on a new device.
              Treat it like a master password.
            </Text>
          </View>

          <Pressable
            onPress={() => setAcknowledged((v) => !v)}
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 10,
              paddingVertical: 8,
            }}
          >
            <View
              style={{
                width: 18,
                height: 18,
                borderWidth: 1,
                borderColor: acknowledged ? semantic.accent : semantic.hairStrong,
                backgroundColor: acknowledged ? semantic.accent : "transparent",
                borderRadius: r.sm,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              {acknowledged ? <Icon.check color="#0a0907" /> : null}
            </View>
            <Text
              style={{
                flex: 1,
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.ink,
              }}
            >
              I've saved my recovery key in a safe place.
            </Text>
          </Pressable>
        </ScrollView>
      </Body>
      <BottomAction>
        <Button
          full
          variant="primary"
          onPress={onContinue}
          disabled={!acknowledged}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          CONTINUE
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(EmergencyKit);
