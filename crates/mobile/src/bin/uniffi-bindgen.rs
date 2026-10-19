#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Build-time tool: generates the Swift bindings from the compiled library. Not shipped.

fn main() {
    uniffi::uniffi_bindgen_main()
}
