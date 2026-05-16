import { Tabs } from "expo-router";
import { TabBar } from "../../components/TabBar";

export default function TabsLayout() {
  return (
    <Tabs
      tabBar={(props) => <TabBar {...props} />}
      screenOptions={{ headerShown: false }}
    >
      <Tabs.Screen name="groups" />
      <Tabs.Screen name="direct" />
      <Tabs.Screen name="search" />
      <Tabs.Screen name="self" />
    </Tabs>
  );
}
