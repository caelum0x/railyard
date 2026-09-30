// Expo Router root layout: one $screen per route change, sent after consent.
import { useEffect } from "react";
import { Stack, usePathname } from "expo-router";
import { createNativeAnalytics, screenTracker } from "@railyard/analytics-js/react-native";

export const analytics = createNativeAnalytics();
const track = screenTracker(analytics);

export default function RootLayout() {
  const path = usePathname();
  useEffect(() => {
    track(path);
  }, [path]);
  return <Stack />;
}
