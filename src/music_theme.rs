//! Version-two application styles. Static assets are shared, decoded on the skin
//! worker and included in the existing combined eight-MiB working reservation.
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 5] = ["header", "library", "list", "player", "video"];
pub const FONT_FAMILIES: [&str; 3] = [
    "DejaVu Sans", "DejaVu Sans Mono", "Mochune Display",
];
pub const STATES: [&str; 8] = [
    "normal", "hover", "pressed", "active", "disabled", "focus", "playing", "loading",
];

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Section {
    pub defaults: Paint,
    pub components: BTreeMap<String, Component>,
}

#[derive(Clone, Default)]
pub struct Component {
    pub paint: Paint,
    pub states: BTreeMap<String, Paint>,
}

impl<'de> Deserialize<'de> for Component {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Serde flatten does not enforce the flattened type's unknown-field
        // policy. Split the one reserved field before validating the paint.
        let mut fields = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let states = fields
            .remove("states")
            .map(serde_json::from_value)
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();
        let paint = serde_json::from_value(serde_json::Value::Object(fields.into_iter().collect()))
            .map_err(serde::de::Error::custom)?;
        Ok(Self { paint, states })
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paint {
    pub background: Option<String>,
    pub gradient_end: Option<String>,
    pub border: Option<String>,
    pub text: Option<String>,
    pub tint: Option<String>,
    pub radius: Option<f32>,
    pub border_width: Option<f32>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub font_weight: Option<f32>,
    pub opacity: Option<f32>,
    pub scale: Option<f32>,
    pub offset_x: Option<f32>,
    pub offset_y: Option<f32>,
    pub inset: Option<f32>,
    pub frame_slice: Option<f32>,
    pub pixelated: Option<bool>,
    pub replace_chrome: Option<bool>,
    pub image: Option<String>,
    pub frame: Option<String>,
}

impl Paint {
    pub fn overlay(&mut self, other: &Self) {
        macro_rules! fields {($($f:ident),*) => {$(if other.$f.is_some() {self.$f = other.$f.clone();})*};}
        fields!(
            background,
            gradient_end,
            border,
            text,
            tint,
            radius,
            border_width,
            font_family,
            font_size,
            font_weight,
            opacity,
            scale,
            offset_x,
            offset_y,
            inset,
            frame_slice,
            pixelated,
            replace_chrome,
            image,
            frame
        );
    }
    pub fn validate(&self, assets: &BTreeMap<String, super::music_skin::Asset>) -> Result<()> {
        for value in [
            &self.background,
            &self.gradient_end,
            &self.border,
            &self.text,
            &self.tint,
        ]
        .into_iter()
        .flatten()
        {
            super::music_skin::color(value)?;
        }
        for (name, value, min, max) in [
            ("radius", self.radius, 0., 64.),
            ("border_width", self.border_width, 0., 12.),
            ("font_size", self.font_size, 8., 32.),
            ("font_weight", self.font_weight, 100., 900.),
            ("opacity", self.opacity, 0., 1.),
            ("scale", self.scale, 0.5, 1.5),
            ("offset_x", self.offset_x, -16., 16.),
            ("offset_y", self.offset_y, -16., 16.),
            ("inset", self.inset, 0., 24.),
            ("frame_slice", self.frame_slice, 0., 64.),
        ] {
            if let Some(v) = value {
                ensure!(
                    v.is_finite() && (min..=max).contains(&v),
                    "{name} must be between {min} and {max}"
                );
            }
        }
        if let Some(font) = &self.font_family {
            ensure!(
                !font.trim().is_empty() && font.len() <= 80 && !font.chars().any(char::is_control),
                "Use a printable font family of at most 80 bytes"
            );
        }
        for name in [&self.image, &self.frame].into_iter().flatten() {
            ensure!(assets.contains_key(name), "Unknown theme artwork: {name}");
        }
        Ok(())
    }
}

pub fn validate(
    sections: &BTreeMap<String, Section>,
    assets: &BTreeMap<String, super::music_skin::Asset>,
) -> Result<()> {
    ensure!(assets.len() <= 40, "At most 40 named theme assets");
    for (section, value) in sections {
        ensure!(
            SECTIONS.contains(&section.as_str()),
            "Unknown theme section: {section}"
        );
        ensure!(
            value.components.len() <= 192,
            "Too many component rules in {section}"
        );
        value.defaults.validate(assets)?;
        for (key, component) in &value.components {
            ensure!(
                !key.is_empty()
                    && key.len() <= 96
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "Invalid component key: {key}"
            );
            component.paint.validate(assets)?;
            for (state, paint) in &component.states {
                ensure!(
                    STATES.contains(&state.as_str()),
                    "Unknown visual state: {state}"
                );
                paint.validate(assets)?;
            }
        }
    }
    Ok(())
}

pub fn resolve(section: &Section, key: &str, kind: &str, state: &str) -> Paint {
    let mut result = section.defaults.clone();
    for name in [kind, key] {
        if let Some(component) = section.components.get(name) {
            result.overlay(&component.paint);
            if let Some(paint) = component.states.get("normal") {
                result.overlay(paint);
            }
            if let Some(paint) = component.states.get(state) {
                result.overlay(paint);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inheritance_and_states_are_deterministic() {
        let section: Section = serde_json::from_value(serde_json::json!({
            "defaults":{"radius":2,"font_family":"DejaVu Sans"},
            "components":{"button":{"radius":4,"states":{"normal":{"opacity":0.9},"hover":{"radius":6}}},"play":{"radius":8,"states":{"hover":{"text":"#abcdef"}}}}
        })).unwrap();
        let p = resolve(&section, "play", "button", "hover");
        assert_eq!(p.radius, Some(8.));
        assert_eq!(p.opacity, Some(0.9));
        assert_eq!(p.text.as_deref(), Some("#abcdef"));
        assert_eq!(p.font_family.as_deref(), Some("DejaVu Sans"));
    }
    #[test]
    fn borderless_chrome_and_state_icons_resolve_independently() {
        let section: Section = serde_json::from_value(serde_json::json!({
            "components": {
                "button": {"border_width": 2, "states": {"hover": {"border_width": 3}}},
                "heart-button": {"border_width": 0, "replace_chrome": true},
                "heart-button-icon": {"image": "heart", "states": {"active": {"image": "liked-heart"}}},
                "play-icon": {"image": "play"},
                "pause-icon": {"image": "pause"}
            }
        })).unwrap();
        for state in STATES {
            let button = resolve(&section, "heart-button", "button", state);
            assert_eq!(button.border_width, Some(0.));
            assert_eq!(button.replace_chrome, Some(true));
        }
        assert_eq!(resolve(&section, "heart-button-icon", "icon", "normal").image.as_deref(), Some("heart"));
        assert_eq!(resolve(&section, "heart-button-icon", "icon", "active").image.as_deref(), Some("liked-heart"));
        assert_eq!(resolve(&section, "play-icon", "icon", "normal").image.as_deref(), Some("play"));
        assert_eq!(resolve(&section, "pause-icon", "icon", "normal").image.as_deref(), Some("pause"));
    }
    #[test]
    fn rejects_unknown_fields_and_unbounded_values() {
        for json in [
            serde_json::json!({"script":"bad"}),
            serde_json::json!({"radius":100}),
            serde_json::json!({"image":"missing"}),
        ] {
            let result = serde_json::from_value::<Paint>(json).and_then(|p| {
                p.validate(&BTreeMap::new())
                    .map(|_| p)
                    .map_err(serde::de::Error::custom)
            });
            assert!(result.is_err());
        }
        assert!(serde_json::from_value::<Component>(serde_json::json!({"script":"bad"})).is_err());
    }
}

pub fn ui_paint(p: Paint, images: &BTreeMap<String, slint::Image>) -> crate::ThemePaint {
    let mut out = crate::ThemePaint::default();
    macro_rules! colors {($($f:ident, $has:ident);*) => {$(if let Some(v) = &p.$f {out.$f = super::music_skin::color(v).unwrap();out.$has = true;})*};}
    colors!(background, has_background; gradient_end, has_gradient_end; border, has_border; text, has_text; tint, has_tint);
    macro_rules! numbers {($($f:ident, $has:ident);*) => {$(if let Some(v) = p.$f {out.$f = v;out.$has = true;})*};}
    numbers!(radius, has_radius; border_width, has_border_width; font_size, has_font_size; font_weight, has_font_weight; opacity, has_opacity);
    if let Some(v) = p.font_family {
        out.font_family = v.into();
        out.has_font_family = true;
    }
    out.scale = p.scale.unwrap_or(1.);
    out.has_scale = p.scale.is_some();
    out.offset_x = p.offset_x.unwrap_or(0.);
    out.offset_y = p.offset_y.unwrap_or(0.);
    out.inset = p.inset.unwrap_or(0.);
    out.frame_slice = p.frame_slice.unwrap_or(0.);
    out.pixelated = p.pixelated.unwrap_or(false);
    out.replace_chrome = p.replace_chrome.unwrap_or(false);
    out.image = p
        .image
        .and_then(|n| images.get(&n).cloned())
        .unwrap_or_default();
    out.frame = p
        .frame
        .and_then(|n| images.get(&n).cloned())
        .unwrap_or_default();
    out.frame_slice = out
        .frame_slice
        .min(out.frame.size().width.min(out.frame.size().height) as f32 / 2.);
    out
}
