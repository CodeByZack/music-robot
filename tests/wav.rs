//! WAV 读侧测试。
//! ⚠️ fixture 由 ffmpeg 从样本现场生成（配方见 HANDOFF.md §8.3），**非手工摆字节**。
use music_robot::tag::read::read_tags;

fn fx(name: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name);
    assert!(p.exists(), "fixture 缺失：{}（重建方式见 HANDOFF.md §8.3）", p.display());
    p
}

#[test]
fn wav_plain_no_tags_reports_audio_props_not_fake_success() {
    // 关键回归：占位实现曾对 WAV 返回空 Ok(…)——那是"假成功"，会让用户误以为文件没标签。
    let m = read_tags(&fx("plain.wav")).expect("无标签 WAV 也必须能正常读取");
    assert_eq!(m.source, "riff");
    assert_eq!(m.sample_rate, 44100, "采样率必须真读到，而非占位的 0");
    assert_eq!(m.bits_per_sample, Some(16));
    assert_eq!(m.duration_ms, 2000);
    assert!(m.artists.is_empty() && m.albums.is_empty(), "无标签就该真的为空（区别于解析失败）");
}

#[test]
fn wav_list_info_maps_all_fields() {
    let m = read_tags(&fx("tagged.wav")).unwrap();
    assert_eq!(m.title.as_deref(), Some("测试标题"));
    assert_eq!(m.artists, vec!["测试歌手".to_string()], "LIST INFO 字段映射不能整体错位");
    assert_eq!(m.albums, vec!["测试专辑".to_string()]);
    assert_eq!(m.year.as_deref(), Some("2019"));
    assert_eq!(m.genres, vec!["Rock".to_string()]);
    assert_eq!(m.comment.as_deref(), Some("测试备注"));
    assert_eq!(m.sample_rate, 44100);
    assert_eq!(m.bits_per_sample, Some(16));
}

#[test]
fn wav_odd_sized_chunk_padding_is_honored() {
    // RIFF 偶对齐：size 为奇数的子 chunk 后跟 1 字节 pad。漏掉 pad 会让后续 chunk id 全部错位。
    let m = read_tags(&fx("oddpad.wav")).unwrap();
    assert_eq!(m.sample_rate, 8000, "8kHz/单声道：若 fmt 之后 chunk 错位这里必错");
    assert_eq!(m.duration_ms, 1000);
    assert_eq!(m.bits_per_sample, Some(16));
    assert_eq!(m.title.as_deref(), Some("奇"), "单字符标题 → INFO 子项长度为奇数，正是 pad 触发条件");
    assert_eq!(m.artists, vec!["短名A".to_string()]);
    assert!(!m.pictures.iter().any(|p| p.data.is_empty()), "不应因错位把垃圾当封面");
}

#[test]
fn wav_native_probe_duration_matches_byte_rate() {
    // 兜底通道：时长 = data 字节 ÷ byteRate，必须精确（WAV 是未压缩格式，不该有 ±40ms 误差）
    use music_robot::tag::read::native_probe::wav_native_probe;
    for (name, want_ms) in [("plain.wav", 2000i64), ("tagged.wav", 2000), ("oddpad.wav", 1000)] {
        let buf = std::fs::read(fx(name)).unwrap();
        let np = wav_native_probe(&buf);
        assert_eq!(np.duration_ms, want_ms, "{name} 本地估算时长应精确");
        assert_eq!(np.sample_rate, if name == "oddpad.wav" { 8000 } else { 44100 }, "{name} 采样率");
        assert_eq!(np.bits_per_sample, Some(16));
    }
}
