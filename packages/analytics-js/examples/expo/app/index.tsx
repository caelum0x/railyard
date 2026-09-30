import { Button, Text, View } from "react-native";
import { Link } from "expo-router";
import { analytics } from "./_layout";

export default function Home() {
  return (
    <View style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 16 }}>
      <Text>Shop</Text>
      <Button title="Allow analytics" onPress={() => analytics.setConsent("granted")} />
      <Button title="Add to cart" onPress={() => analytics.capture("added_to_cart", { sku: "tee-1" })} />
      <Link href="/cart">Cart</Link>
    </View>
  );
}
