import { Stack } from "expo-router";

// No swipe-to-go-back in auth — it would lose flow progress.
export default function AuthLayout() {
  return (
    <Stack screenOptions={{ headerShown: false, gestureEnabled: false }}>
      {/* PIN → Initializing is an instant handoff (the loading setup screen),
          so it shouldn't slide — it cuts straight in with no animation. The
          subsequent Initializing → tabs hop fades (configured on the root
          (tabs) screen). Other auth screens keep the default transition. */}
      <Stack.Screen name="initializing" options={{ animation: "none" }} />
    </Stack>
  );
}
