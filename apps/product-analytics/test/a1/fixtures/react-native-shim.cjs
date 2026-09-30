// A minimal stand-in for the `react-native` module so the unmodified React
// Native SDK runs under Node in tests: an iOS device that is in the
// foreground. Only what the SDK touches at runtime is real; UI components
// are names (nothing renders in these tests).
const listeners = new Set();
const component = (name) => name;
module.exports = {
  Platform: { OS: "ios", Version: "17.4", isPad: false, isTV: false, constants: {}, select: (o) => o.ios ?? o.default },
  AppState: {
    currentState: "active",
    addEventListener: (_type, fn) => {
      listeners.add(fn);
      return { remove: () => listeners.delete(fn) };
    },
    _emit: (state) => {
      module.exports.AppState.currentState = state;
      for (const fn of listeners) fn(state);
    },
  },
  Dimensions: { get: () => ({ width: 390, height: 844, scale: 3, fontScale: 1 }), addEventListener: () => ({ remove() {} }) },
  Keyboard: { addListener: () => ({ remove() {} }), dismiss() {} },
  Linking: { getInitialURL: async () => null, addEventListener: () => ({ remove() {} }) },
  UIManager: { getViewManagerConfig: () => null, hasViewManagerConfig: () => false },
  NativeModules: {},
  StyleSheet: { create: (s) => s, flatten: (s) => s, hairlineWidth: 1, absoluteFill: {} },
  useWindowDimensions: () => ({ width: 390, height: 844, scale: 3, fontScale: 1 }),
  View: component("View"),
  Text: component("Text"),
  TextInput: component("TextInput"),
  TouchableOpacity: component("TouchableOpacity"),
  Pressable: component("Pressable"),
  ScrollView: component("ScrollView"),
  Modal: component("Modal"),
  KeyboardAvoidingView: component("KeyboardAvoidingView"),
};
