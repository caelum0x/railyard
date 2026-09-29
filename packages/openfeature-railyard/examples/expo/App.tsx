// Example Expo app (Wave P PY2.1 acceptance 1): the banner follows the
// `new_checkout` flag. Flip it in the console; the app shows the change within
// 60 s with no new build. EXPO_PUBLIC_FLAGS_KEY is written to .env when
// flags are turned on for the project.
import AsyncStorage from "@react-native-async-storage/async-storage";
import { useEffect, useState } from "react";
import { Text, View } from "react-native";
import { createFlagsClient } from "@railyard/openfeature";

const flags = createFlagsClient({ key: process.env.EXPO_PUBLIC_FLAGS_KEY ?? "", storage: AsyncStorage });

export default function App() {
  const [on, setOn] = useState(flags.isEnabled("new_checkout"));
  useEffect(() => {
    void flags.ready().then(() => setOn(flags.isEnabled("new_checkout")));
    return flags.subscribe(() => setOn(flags.isEnabled("new_checkout")));
  }, []);
  return (
    <View style={{ flex: 1, alignItems: "center", justifyContent: "center" }}>
      <Text>{on ? "New checkout is on" : "Classic checkout"}</Text>
    </View>
  );
}
