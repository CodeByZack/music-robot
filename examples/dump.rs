use music_tag::tag::read::read_tags;
fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut fs: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("mp3") | Some("flac"))).collect();
    fs.sort();
    for p in &fs {
        match read_tags(p) {
            Ok(m) => println!("{:<58} dur={:<9} src={:<7} pics={} frames={} title={:?} artist={:?}",
                p.file_name().unwrap().to_string_lossy(), m.duration_ms, m.source, m.pictures.len(), m.raw_frames.len(),
                m.title, m.artists.first()),
            Err(e) => println!("{:<58} ERR {}", p.file_name().unwrap().to_string_lossy(), e),
        }
    }
}
