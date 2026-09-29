use music_robot::tag::read::read_tags;
fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut fs: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("mp3") | Some("flac"))).collect();
    fs.sort();
    println!("--- 强制本地兜底通道（MR_NO_FFPROBE=1）---");
    for p in &fs {
        match read_tags(p) {
            Ok(m) => println!("{:<46} dur={:<8} sr={:<7} bits={:?} br={:?}",
                p.file_name().unwrap().to_string_lossy(), m.duration_ms, m.sample_rate, m.bits_per_sample, m.bitrate_bps),
            Err(e) => println!("{:<46} ERR {}", p.file_name().unwrap().to_string_lossy(), e),
        }
    }
}
