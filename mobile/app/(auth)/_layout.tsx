import { Stack } from "expo-router";

// No swipe-to-go-back in auth — it would lose flow progress.
export default function AuthLayout() {
  return (
    <Stack screenOptions={{ headerShown: false, gestureEnabled: false }}>
      {/* PIN → Initializing is an instant handoff, so it cuts in unanimated. */}
      <Stack.Screen name="initializing" options={{ animation: "none" }} />
    </Stack>
  );
}
