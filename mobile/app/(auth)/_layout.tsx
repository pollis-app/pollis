import { Stack } from "expo-router";

// No swipe-to-go-back in auth — it would lose flow progress.
export default function AuthLayout() {
  return (
    <Stack screenOptions={{ headerShown: false, gestureEnabled: false }} />
  );
}
