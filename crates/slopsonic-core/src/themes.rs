use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Palette {
    colors: BTreeMap<String, String>,
}

pub fn colors(dark: bool) -> BTreeMap<String, String> {
    let json = if dark {
        include_str!("../../../resources/themes/dark.json")
    } else {
        include_str!("../../../resources/themes/light.json")
    };
    serde_json::from_str::<Palette>(json)
        .expect("bundled theme must be valid")
        .colors
}
