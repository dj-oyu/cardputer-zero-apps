//! `Canvas` の PNG 出力と、render-test の正解画像との比較。
//!
//! 正解画像は `zcode/tests/golden/` に置く。[`check_golden`] は:
//!
//! - 正解が無ければ、今の描画結果を正解として書き出す（目で確かめてからコミットする）
//! - 環境変数 `ZCODE_UPDATE_GOLDEN=1` なら、正解を今の描画結果で置き換える
//! - 違っていれば、隣に `<名前>.actual.png` を書き出してエラーを返す
//!
//! 正解画像の運用（一覧・差分画像など）を整えるのは後の段階の作業。ここは
//! 「比べる、無ければ書く」だけの最小版。

use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use zc_core::{Canvas, Color};

fn to_io(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// `Canvas` を PNG（RGB 8bit）にする。
pub fn encode_png(canvas: &Canvas, out: impl io::Write) -> io::Result<()> {
    let mut enc = png::Encoder::new(out, canvas.width() as u32, canvas.height() as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Best);
    let mut wr = enc.write_header().map_err(to_io)?;
    let mut rgb = Vec::with_capacity(canvas.pixels().len() * 3);
    for &p in canvas.pixels() {
        rgb.extend_from_slice(&Color(p).to_rgb8());
    }
    wr.write_image_data(&rgb).map_err(to_io)?;
    wr.finish().map_err(to_io)
}

/// `Canvas` を PNG ファイルに書く。
pub fn write_png(canvas: &Canvas, path: impl AsRef<Path>) -> io::Result<()> {
    if let Some(dir) = path.as_ref().parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    encode_png(canvas, BufWriter::new(File::create(path)?))
}

/// PNG ファイル（RGB または RGBA の 8bit）を `Canvas` に読む。
pub fn read_png(path: impl AsRef<Path>) -> io::Result<Canvas> {
    let dec = png::Decoder::new(BufReader::new(File::open(path)?));
    let mut rd = dec.read_info().map_err(to_io)?;
    let mut buf = vec![0u8; rd.output_buffer_size()];
    let info = rd.next_frame(&mut buf).map_err(to_io)?;
    let n = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
        (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
        other => return Err(to_io(format!("PNG の形式が想定外: {other:?}"))),
    };
    let mut c = Canvas::with_size(info.width as u16, info.height as u16);
    for (d, s) in c.pixels_mut().iter_mut().zip(buf.chunks_exact(n)) {
        *d = Color::rgb(s[0], s[1], s[2]).0;
    }
    Ok(c)
}

/// 正解画像と比べる。使い方はモジュールの説明を参照。
pub fn check_golden(canvas: &Canvas, golden: impl AsRef<Path>) -> Result<(), String> {
    let golden = golden.as_ref();
    let update = std::env::var_os("ZCODE_UPDATE_GOLDEN").is_some_and(|v| !v.is_empty() && v != "0");
    if update || !golden.exists() {
        write_png(canvas, golden).map_err(|e| format!("{}: {e}", golden.display()))?;
        eprintln!("正解画像を書いた: {}", golden.display());
        return Ok(());
    }
    let want = read_png(golden).map_err(|e| format!("{}: {e}", golden.display()))?;
    let actual_path = actual_path(golden);
    if (want.width(), want.height()) != (canvas.width(), canvas.height()) {
        let _ = write_png(canvas, &actual_path);
        return Err(format!(
            "{}: 大きさが違う（正解 {}x{}、描画 {}x{}）",
            golden.display(),
            want.width(),
            want.height(),
            canvas.width(),
            canvas.height()
        ));
    }
    let w = canvas.width() as usize;
    let diff: Vec<usize> =
        want.pixels().iter().zip(canvas.pixels()).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
    if diff.is_empty() {
        let _ = std::fs::remove_file(&actual_path);
        return Ok(());
    }
    let _ = write_png(canvas, &actual_path);
    let (first, last) = (diff[0], diff[diff.len() - 1]);
    Err(format!(
        "{}: {} 画素が違う（行 {}〜{}、最初は x={} y={}）。描画結果: {}。正しければ ZCODE_UPDATE_GOLDEN=1 で更新する",
        golden.display(),
        diff.len(),
        first / w,
        last / w,
        first % w,
        first / w,
        actual_path.display()
    ))
}

fn actual_path(golden: &Path) -> PathBuf {
    golden.with_extension("actual.png")
}

#[cfg(test)]
mod tests {
    use super::*;
    use zc_core::Rect;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zc-display-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn png_roundtrip_keeps_rgb565() {
        let mut c = Canvas::new();
        for (i, p) in c.pixels_mut().iter_mut().enumerate() {
            *p = ((i as u32).wrapping_mul(2654435761) >> 16) as u16;
        }
        let p = tmp("rt.png");
        write_png(&c, &p).unwrap();
        assert_eq!(read_png(&p).unwrap(), c);
    }

    #[test]
    fn golden_write_then_compare() {
        let p = tmp("golden.png");
        let _ = std::fs::remove_file(&p);
        let mut c = Canvas::with_size(16, 8);
        c.fill_rect(Rect::new(2, 2, 4, 4), Color::WHITE);
        assert!(check_golden(&c, &p).is_ok()); // 無いので書く
        assert!(check_golden(&c, &p).is_ok()); // 同じ
        c.put(10, 5, Color::WHITE);
        let err = check_golden(&c, &p).unwrap_err();
        assert!(err.contains("1 画素"), "{err}");
        assert!(p.with_extension("actual.png").exists());
    }
}
