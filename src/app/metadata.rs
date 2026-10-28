//! Stream helper metadata and discard unused bulk before allocating the retained JSON tree.
use anyhow::{Context, Result, ensure};
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::{fmt, fs::File, io::BufReader, path::Path};
const MAX_INPUT: u64 = 64 * 1024 * 1024;
struct Compact(Value);
impl<'de> Deserialize<'de> for Compact {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(CompactVisitor)
    }
}
struct CompactVisitor;
impl<'de> Visitor<'de> for CompactVisitor {
    type Value = Compact;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("YouTube metadata")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Compact, E> {
        Ok(Compact(Value::Null))
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Compact, E> {
        Ok(Compact(v.into()))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Compact, E> {
        Ok(Compact(v.into()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Compact, E> {
        Ok(Compact(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Compact, E> {
        Ok(Compact(
            Number::from_f64(v)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        ))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Compact, E> {
        Ok(Compact(v.into()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Compact, E> {
        Ok(Compact(v.into()))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Compact, A::Error> {
        let mut result = Vec::new();
        while let Some(v) = a.next_element::<Compact>()? {
            if result.len() >= 4096 {
                return Err(de::Error::custom("Too many metadata entries"));
            }
            result.push(v.0);
        }
        Ok(Compact(Value::Array(result)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Compact, A::Error> {
        let mut result = Map::new();
        while let Some(key) = a.next_key::<String>()? {
            if matches!(
                key.as_str(),
                "fragments" | "heatmap" | "thumbnails" | "requested_downloads"
            ) {
                a.next_value::<IgnoredAny>()?;
                continue;
            }
            let value = if matches!(key.as_str(), "automatic_captions" | "subtitles") {
                a.next_value_seed(Captions {
                    auto: key == "automatic_captions",
                })?
            } else {
                a.next_value::<Compact>()?.0
            };
            if result.len() >= 4096 {
                return Err(de::Error::custom("Too many metadata fields"));
            }
            result.insert(key, value);
        }
        Ok(Compact(Value::Object(result)))
    }
}
struct Captions {
    auto: bool,
}
impl<'de> DeserializeSeed<'de> for Captions {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Captions {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("caption languages")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
        let mut languages = Map::new();
        while let Some(language) = a.next_key::<String>()? {
            if language == "live_chat" || languages.len() >= 64 {
                a.next_value::<IgnoredAny>()?;
                continue;
            }
            let formats = a.next_value_seed(CaptionFormats { auto: self.auto })?;
            if !formats.is_empty() {
                languages.insert(language, Value::Array(formats));
            }
        }
        Ok(Value::Object(languages))
    }
}
struct CaptionFormats {
    auto: bool,
}
impl<'de> DeserializeSeed<'de> for CaptionFormats {
    type Value = Vec<Value>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for CaptionFormats {
    type Value = Vec<Value>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("caption formats")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(Vec::new())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let mut result = Vec::new();
        while let Some(Compact(format)) = a.next_element::<Compact>()? {
            let url = format["url"].as_str().unwrap_or("");
            if result.is_empty()
                && format["ext"] == "vtt"
                && url.starts_with("https://")
                && !url.contains("/api/manifest/")
                && (!self.auto || (!url.contains("&tlang=") && !url.contains("?tlang=")))
            {
                result.push(format);
            }
        }
        Ok(result)
    }
}
pub(super) fn read(path: &Path) -> Result<Value> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_INPUT,
        "YouTube metadata exceeded the 64 MiB input limit"
    );
    let mut decoder =
        serde_json::Deserializer::from_reader(BufReader::with_capacity(32 * 1024, file));
    let value = Compact::deserialize(&mut decoder)
        .context("YouTube returned an unreadable response")?
        .0;
    decoder
        .end()
        .context("Unexpected content after YouTube metadata")?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captions_preserve_original_vtt_and_skip_translated_and_unused_payloads() {
        let raw = serde_json::json!({"id":"abcdefghijk","formats":[{"format_id":"v","url":"https://example.org/video","fragments":[{"url":"unused"}]}],"heatmap":[{"start":0}],"automatic_captions":{"de":[{"ext":"vtt","url":"https://example.org/caption?tlang=de"}],"en":[{"ext":"json3","url":"https://example.org/json"},{"ext":"vtt","url":"https://example.org/en","name":"English"}]},"subtitles":{"ja":[{"ext":"vtt","url":"https://example.org/ja"}]}});
        let Compact(v) = serde_json::from_value::<Compact>(raw).unwrap();
        assert_eq!(v["automatic_captions"].as_object().unwrap().len(), 1);
        assert_eq!(v["automatic_captions"]["en"].as_array().unwrap().len(), 1);
        assert_eq!(v["subtitles"]["ja"][0]["ext"], "vtt");
        assert!(v["formats"][0].get("fragments").is_none());
        assert!(v.get("heatmap").is_none());
        assert_eq!(v["formats"][0]["url"], "https://example.org/video");
    }
    #[test]
    fn valid_metadata_larger_than_the_old_limit_is_not_truncated() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("duck-large-metadata-{}.json", std::process::id()));
        {
            let mut file = std::io::BufWriter::new(File::create(&path).unwrap());
            file.write_all(br#"{"id":"abcdefghijk","heatmap":""#)
                .unwrap();
            let chunk = [b'x'; 8192];
            for _ in 0..1100 {
                file.write_all(&chunk).unwrap();
            }
            file.write_all(
                br#"","title":"Still plays","formats":[{"url":"https://example.org/video"}]}"#,
            )
            .unwrap();
        }
        let value = read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(value["title"], "Still plays");
        assert!(serde_json::to_vec(&value).unwrap().len() < 512);
    }
}

/// Formats can carry their own headers even when the metadata root has none.
pub(super) fn stream_headers<'a>(root: &'a Value, format: &'a Value) -> Vec<(&'a str, &'a str)> {
    let mut headers: Vec<(&str, &str)> = Vec::new();
    for info in [root, format] {
        if let Some(map) = info["http_headers"].as_object() {
            for (key, value) in map {
                let Some(value) = value.as_str() else {
                    continue;
                };
                if key.len() > 128
                    || value.len() > 8192
                    || key.contains(['\r', '\n'])
                    || value.contains(['\r', '\n'])
                {
                    continue;
                }
                if let Some(old) = headers
                    .iter_mut()
                    .find(|(name, _)| name.eq_ignore_ascii_case(key))
                {
                    *old = (key, value);
                } else if headers.len() < 32 {
                    headers.push((key, value));
                }
            }
        }
    }
    headers
}
#[cfg(test)]
mod header_tests {
    use super::*;
    #[test]
    fn distinct_stream_headers_override_root_case_insensitively() {
        let root = serde_json::json!({"http_headers":{"User-Agent":"root", "Accept":"media"}});
        let video = serde_json::json!({"http_headers":{"user-agent":"video"}});
        let audio = serde_json::json!({"http_headers":{"User-Agent":"audio", "Bad":"x\r\ny"}});
        let v = stream_headers(&root, &video);
        assert_eq!(v.len(), 2);
        assert!(v.contains(&("user-agent", "video")));
        let a = stream_headers(&root, &audio);
        assert_eq!(a.len(), 2);
        assert!(a.contains(&("User-Agent", "audio")));
        assert!(stream_headers(&Value::Null, &video).contains(&("user-agent", "video")));
    }
}
