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
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useCreateGroup } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function NewGroup() {
  const router = useRouter();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const createGroup = useCreateGroup();
  const setSelectedGroupId = appStore.setSelectedGroupId;

  const onSubmit = () => {
    const trimmedName = name.trim();
    if (!trimmedName) {
      return;
    }
    createGroup.mutate(
      {
        name: trimmedName,
        description: description.trim() || undefined,
        createDefaultTextChannel: true,
      },
      {
        onSuccess: (group) => {
          setSelectedGroupId(group.id);
          // The default text channel created server-side is fetched
          // lazily by the groups list; land the user on the group page
          // so they see the new channel render in once the query
          // refetches.
          router.replace({
            pathname: "/group/[id]",
            params: { id: group.id },
          });
        },
      },
    );
  };

  return (
    <Screen>
      <Crumb segs={[{ label: "GROUPS" }, { label: "New", leaf: true }]} />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 16 }}>
          <View style={{ gap: 8 }}>
            <Text style={ty.label}>GROUP NAME</Text>
            <Field
              amber
              value={name}
              onChangeText={setName}
              placeholder="Quick Group"
              icon={<Icon.people color={semantic.mute} />}
            />
          </View>
          <View style={{ gap: 8 }}>
            <Text style={ty.label}>DESCRIPTION (OPTIONAL)</Text>
            <Field
              value={description}
              onChangeText={setDescription}
              placeholder="What's this group for?"
            />
          </View>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            A #General text channel is created automatically. You can add more
            later. Group metadata is visible to invited members only.
          </Text>
          {createGroup.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
              }}
            >
              {(createGroup.error as Error).message ||
                "Couldn't create the group."}
            </Text>
          ) : null}
        </View>
      </Body>
      <BottomAction>
        <Button
          full
          variant="primary"
          onPress={onSubmit}
          disabled={!name.trim() || createGroup.isPending}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {createGroup.isPending ? "CREATING…" : "CREATE GROUP"}
        </Button>
        <Button variant="subtle" full onPress={() => router.back()}>
          Cancel
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(NewGroup);
