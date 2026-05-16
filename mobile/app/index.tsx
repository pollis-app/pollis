import { Redirect } from "expo-router";

// Stub: always start at the auth flow. Real builds would branch on a
// persisted session in secure-store.
export default function Index() {
  return <Redirect href="/(auth)/email" />;
}
