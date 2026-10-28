//! Inspect retained metadata size without printing signed stream/caption URLs.
#[path = "../src/app/metadata.rs"]
mod compact;
fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("Pass a yt-dlp JSON file"))?,
    );
    let value = compact::read(&path)?;
    println!(
        "{}",
        serde_json::json!({"input_bytes":std::fs::metadata(&path)?.len(),"retained_json_bytes":serde_json::to_vec(&value)?.len(),"formats":value["formats"].as_array().map(Vec::len),"original_auto_caption_languages":value["automatic_captions"].as_object().map(serde_json::Map::len),"subtitle_languages":value["subtitles"].as_object().map(serde_json::Map::len)})
    );
    Ok(())
}
