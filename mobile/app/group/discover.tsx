import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
  Card,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useGroupBySlug, useRequestGroupAccess, useMyJoinRequest } from "../../hooks/queries";

export default function Discover() {
  const router = useRouter();
  const [slug, setSlug] = useState("");
  const search = useGroupBySlug(slug.trim().replace(/^#/, ""));
  const requestAccess = useRequestGroupAccess();
  const myRequest = useMyJoinRequest(search.data?.id ?? null);

  const onRequest = () => {
    if (!search.data) {
      return;
    }
    requestAccess.mutate(search.data.id);
  };

  const status = myRequest.data?.status;

  return (
    <Screen>
      <Crumb segs={[{ label: "GROUPS" }, { label: "Discover", leaf: true }]} />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 8 }}>
          <Text style={ty.label}>GROUP SLUG</Text>
          <Field
            amber
            value={slug}
            onChangeText={setSlug}
            placeholder="#general-room"
            icon={<Icon.diamond size={14} color={semantic.mute} />}
          />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            Slugs are short, unique identifiers chosen by group owners.
            Ask for one to join — there's no public directory.
          </Text>
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          {search.isLoading && slug.trim().length >= 2 ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.mute,
              }}
            >
              Looking up…
            </Text>
          ) : null}
          {search.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.danger,
              }}
            >
              {(search.error as Error).message || "No group with that slug."}
            </Text>
          ) : null}
          {search.data ? (
            <Card>
              <Text
                style={{
                  fontFamily: ty.h1.fontFamily,
                  fontSize: 18,
                  color: semantic.ink,
                }}
              >
                {search.data.name}
              </Text>
              {search.data.description ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 13,
                    color: semantic.mute,
                    marginTop: 4,
                  }}
                >
                  {search.data.description}
                </Text>
              ) : null}
              <View style={{ paddingTop: 12 }}>
                {status === "pending" ? (
                  <Text
                    style={[ty.label, { color: semantic.accent }]}
                  >
                    REQUEST PENDING
                  </Text>
                ) : status === "approved" ? (
                  <Text style={[ty.label, { color: semantic.accent }]}>
                    APPROVED — OPEN GROUPS TAB
                  </Text>
                ) : status === "rejected" ? (
                  <Text style={[ty.label, { color: semantic.danger }]}>
                    REQUEST DECLINED
                  </Text>
                ) : (
                  <Button
                    full
                    variant="primary"
                    onPress={onRequest}
                    disabled={requestAccess.isPending}
                    iconRight={<Icon.arrowRight color="#0a0907" />}
                  >
                    {requestAccess.isPending ? "REQUESTING…" : "REQUEST TO JOIN"}
                  </Button>
                )}
              </View>
            </Card>
          ) : null}
        </View>
      </Body>
      <Ctx cr="GROUPS" name="Discover" />
      <BottomAction>
        <Button
          full
          variant="subtle"
          onPress={() => router.back()}
          icon={<Icon.back color={semantic.ink} />}
        >
          Back
        </Button>
      </BottomAction>
    </Screen>
  );
}
