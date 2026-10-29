import { Stack } from "expo-router";

export default function AuthLayout() {
  return (
    <Stack screenOptions={{ headerShown: false, gestureEnabled: false }}>
      {/* PIN → Initializing cuts in with no animation. */}
      <Stack.Screen name="initializing" options={{ animation: "none" }} />
    </Stack>
  );
}
