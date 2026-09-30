import { Button, View } from "react-native";
import { analytics } from "./_layout";

export default function Cart() {
  return (
    <View style={{ flex: 1, alignItems: "center", justifyContent: "center" }}>
      <Button title="Check out" onPress={() => analytics.capture("checkout_started")} />
    </View>
  );
}
