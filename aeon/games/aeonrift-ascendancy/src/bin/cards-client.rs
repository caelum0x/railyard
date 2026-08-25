//! Desktop entry point for the Aeonrift: Ascendancy Bevy touch client.
//!
//! Mobile (iOS/Android) uses the `#[bevy_main]`-annotated `cards::client::main`
//! (loaded as a `cdylib`); this thin binary calls the same function so desktop and
//! mobile share one code path. Only built with `--features client`.

fn main() {
    cards::client::main();
}
