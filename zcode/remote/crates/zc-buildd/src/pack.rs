//! 成果物の仕上げ（docs/stage3-design.md §4.5）。課題 B2。
//!
//! 1. 実行ファイルなら `aarch64-linux-gnu-strip --strip-all`（JS はそのまま）
//! 2. 大きさが [`zc_wire::limits::MAX_ARTIFACT_BYTES`] を超えたら失敗（実機の tmpfs に置けない）
//! 3. SHA-256 と大きさを取る（展開後のもの。実機が確かめる）
//! 4. `zstd -19 --zstd=wlog=21`（**窓は 2MiB まで**: 実機の展開メモリを抑える。圧縮は
//!    x86 側の仕事なので高い圧縮率でよい）で `<out_dir>/artifact.zst` に書く。`zstd` コマンドを
//!    使う（C のライブラリをリンクしない）
//!
//! 実行ファイルが aarch64 の ELF であること（`e_machine` = 183）も確かめる
//! （ホスト用を間違って返さない）。
//!
//! 圧縮したあと、zstd のフレームの頭を読んで窓が 2MiB 以下であることも確かめる（`zstd` の
//! 版によって指定が効かないことがあっても、実機に大きな窓の流れを送らない）。strip は
//! `-o` で別のファイルに書く（cargo の出力を書き換えない。次のビルドで作り直させない）。

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use zc_wire::msg::ArtifactKind;

/// 仕上がった成果物。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub kind: ArtifactKind,
    /// 実機で置く名前（`foo`・`main.js`）。
    pub name: String,
    /// 展開後の SHA-256（16 進）と大きさ。
    pub sha256: String,
    pub bytes: u64,
    /// 圧縮したファイルと、その大きさ。
    pub zpath: PathBuf,
    pub zbytes: u64,
}

/// 仕上げの失敗（人に見せる 1 行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError(pub String);

/// 圧縮した成果物のファイル名（`out_dir` の中）。
pub const ZST_NAME: &str = "artifact.zst";
/// aarch64 の `e_machine`。
const EM_AARCH64: u16 = 183;

/// `file` を仕上げて `out_dir` に置く。
pub fn pack(file: &Path, kind: ArtifactKind, name: &str, out_dir: &Path) -> Result<Packed, PackError> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." || name.bytes().any(|b| b < 0x20) {
        return Err(PackError(format!("成果物の名前が正しくない: {name:?}")));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| PackError(format!("成果物のフォルダを作れない: {e}")))?;
    let stripped = out_dir.join("artifact.stripped");
    let zpath = out_dir.join(ZST_NAME);
    let _ = std::fs::remove_file(&zpath);

    let input: PathBuf = match kind {
        ArtifactKind::Exe => {
            check_aarch64_elf(file)?;
            let strip = crate::runner::find_in_path("aarch64-linux-gnu-strip").ok_or_else(|| PackError("aarch64-linux-gnu-strip が無い".into()))?;
            let out = Command::new(strip)
                .arg("--strip-all")
                .arg("-o")
                .arg(&stripped)
                .arg(file)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| PackError(format!("strip を起動できない: {e}")))?;
            if !out.status.success() {
                let _ = std::fs::remove_file(&stripped);
                return Err(PackError(format!("strip に失敗した: {}", String::from_utf8_lossy(&out.stderr).trim())));
            }
            stripped.clone()
        }
        ArtifactKind::Js => file.to_path_buf(),
    };
    let result = finish(&input, kind, name, &zpath);
    let _ = std::fs::remove_file(&stripped);
    if result.is_err() {
        let _ = std::fs::remove_file(&zpath);
    }
    result
}

fn finish(input: &Path, kind: ArtifactKind, name: &str, zpath: &Path) -> Result<Packed, PackError> {
    let bytes = std::fs::metadata(input).map_err(|e| PackError(format!("成果物を読めない: {e}")))?.len();
    let max = zc_wire::limits::MAX_ARTIFACT_BYTES;
    if bytes > max {
        return Err(PackError(format!("成果物が大きすぎる（{} KiB。上限 {} KiB）", bytes >> 10, max >> 10)));
    }
    let sha256 = sha256_file(input).map_err(|e| PackError(format!("成果物を読めない: {e}")))?;

    let zstd = crate::runner::find_in_path("zstd").ok_or_else(|| PackError("zstd が無い".into()))?;
    let out = Command::new(zstd)
        .args(["-q", "-f", "-19", "-T1", "--no-progress"])
        .arg(format!("--zstd=wlog={}", zc_wire::limits::ZSTD_WINDOW_LOG))
        .arg("-o")
        .arg(zpath)
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| PackError(format!("zstd を起動できない: {e}")))?;
    if !out.status.success() {
        return Err(PackError(format!("zstd に失敗した: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let mut head = [0u8; 18];
    let n = std::fs::File::open(zpath).and_then(|mut f| read_up_to(&mut f, &mut head)).map_err(|e| PackError(format!("圧縮したファイルを読めない: {e}")))?;
    let window = zstd_window_size(&head[..n]).ok_or_else(|| PackError("zstd の頭が読めない".into()))?;
    if window > 1u64 << zc_wire::limits::ZSTD_WINDOW_LOG {
        return Err(PackError(format!("zstd の窓が大きすぎる（{window} バイト）")));
    }
    let zbytes = std::fs::metadata(zpath).map_err(|e| PackError(format!("圧縮したファイルを読めない: {e}")))?.len();
    Ok(Packed { kind, name: name.to_string(), sha256, bytes, zpath: zpath.to_path_buf(), zbytes })
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
    }
    Ok(crate::store::hex(&h.finalize()))
}

/// 64 ビット・リトルエンディアン・aarch64 の ELF の実行ファイル（ET_EXEC / ET_DYN）か。
fn check_aarch64_elf(file: &Path) -> Result<(), PackError> {
    let mut head = [0u8; 20];
    let n = std::fs::File::open(file).and_then(|mut f| read_up_to(&mut f, &mut head)).map_err(|e| PackError(format!("実行ファイルを読めない: {e}")))?;
    if n < 20 || &head[..4] != b"\x7fELF" {
        return Err(PackError("ELF の実行ファイルではない".into()));
    }
    let machine = u16::from_le_bytes([head[18], head[19]]);
    if head[4] != 2 || head[5] != 1 || machine != EM_AARCH64 {
        let what = match machine {
            62 => "x86-64".to_string(),
            m => format!("e_machine={m}"),
        };
        return Err(PackError(format!("aarch64 の実行ファイルではない（{what}）")));
    }
    let etype = u16::from_le_bytes([head[16], head[17]]);
    if etype != 2 && etype != 3 {
        return Err(PackError("実行ファイルではない（ELF の種類が違う）".into()));
    }
    Ok(())
}

/// zstd のフレームの頭（最初のフレーム）から、展開に要る窓の大きさを読む
/// （RFC 8878 §3.1.1.1）。窓の記述が無い（Single_Segment）ときは中身の大きさ。
pub(crate) fn zstd_window_size(head: &[u8]) -> Option<u64> {
    if head.len() < 5 || head[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
        return None;
    }
    let fhd = head[4];
    let single = fhd & 0x20 != 0;
    let fcs_flag = fhd >> 6;
    let dict_len = [0usize, 1, 2, 4][(fhd & 3) as usize];
    let mut pos = 5;
    if !single {
        let wd = *head.get(pos)?;
        let exp = (wd >> 3) as u32;
        let mantissa = (wd & 7) as u64;
        let base = 1u64 << (10 + exp);
        return Some(base + (base / 8) * mantissa);
    }
    pos += dict_len;
    let fcs_len = match fcs_flag {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let b = head.get(pos..pos + fcs_len)?;
    let mut v = 0u64;
    for (i, x) in b.iter().enumerate() {
        v |= (*x as u64) << (8 * i);
    }
    if fcs_len == 2 {
        v += 256;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::testutil::{skip_without, TempDir};

    fn sh(script: &str) -> bool {
        Command::new("sh").arg("-c").arg(script).status().map(|s| s.success()).unwrap_or(false)
    }

    #[test]
    fn window_header_is_read() {
        // 窓の記述あり: exp=11 → 2MiB、mantissa 0
        assert_eq!(zstd_window_size(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, 11 << 3]), Some(2 << 20));
        // 8MiB（-19 の既定。これは断る大きさ）
        assert_eq!(zstd_window_size(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, 13 << 3]), Some(8 << 20));
        // Single_Segment・FCS 2 バイト（+256）
        assert_eq!(zstd_window_size(&[0x28, 0xb5, 0x2f, 0xfd, 0x60, 0x00, 0x01]), Some(256 + 256));
        // Single_Segment・FCS 1 バイト
        assert_eq!(zstd_window_size(&[0x28, 0xb5, 0x2f, 0xfd, 0x20, 100]), Some(100));
        assert_eq!(zstd_window_size(b"nope"), None);
    }

    #[test]
    fn host_executable_is_refused() {
        let t = TempDir::new("pack-host");
        // 試験を走らせている実行ファイル自体（x86_64）
        let me = std::env::current_exe().unwrap();
        let e = pack(&me, ArtifactKind::Exe, "me", t.path()).unwrap_err();
        assert!(e.0.contains("aarch64 の実行ファイルではない"), "{e:?}");
        let txt = t.path().join("a.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(pack(&txt, ArtifactKind::Exe, "a", t.path()).unwrap_err().0.contains("ELF"));
        assert!(pack(&txt, ArtifactKind::Js, "../x", t.path()).is_err());
        assert!(!t.path().join(ZST_NAME).exists());
    }

    #[test]
    fn oversized_is_refused() {
        let t = TempDir::new("pack-big");
        let big = t.path().join("big.js");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(zc_wire::limits::MAX_ARTIFACT_BYTES + 1).unwrap();
        let e = pack(&big, ArtifactKind::Js, "big.js", &t.path().join("out")).unwrap_err();
        assert!(e.0.contains("大きすぎる"), "{e:?}");
    }

    #[test]
    fn js_round_trips_without_strip() {
        if skip_without(&["zstd"]) {
            return;
        }
        let t = TempDir::new("pack-js");
        let js = t.path().join("main.js");
        // 2MiB を超える中身（窓の記述が付く大きさ）
        let mut text = String::new();
        let mut i = 0u64;
        while text.len() < (3 << 20) {
            text.push_str(&format!("console.log({i} * {});\n", i.wrapping_mul(2654435761) % 9973));
            i += 1;
        }
        std::fs::write(&js, &text).unwrap();
        let out = t.path().join("out");
        let p = pack(&js, ArtifactKind::Js, "main.js", &out).unwrap();
        assert_eq!(p.bytes, text.len() as u64);
        assert_eq!(p.sha256, sha256_file(&js).unwrap());
        assert!(p.zbytes > 0 && p.zbytes < p.bytes);
        let back = t.path().join("back.js");
        assert!(sh(&format!("zstd -q -d -o {} {}", back.display(), p.zpath.display())));
        assert_eq!(std::fs::read(&back).unwrap(), text.as_bytes());
        let mut head = [0u8; 18];
        let n = read_up_to(&mut std::fs::File::open(&p.zpath).unwrap(), &mut head).unwrap();
        assert!(zstd_window_size(&head[..n]).unwrap() <= 2 << 20);
        // zstd -l -v の Window Size でも確かめる
        let lv = Command::new("zstd").args(["-l", "-v"]).arg(&p.zpath).output().unwrap();
        let lv = String::from_utf8_lossy(&lv.stdout);
        assert!(lv.contains("Window Size: 2.00 MiB") || lv.contains("Window Size: 2 MiB") || lv.contains("2097152"), "{lv}");
    }

    #[test]
    fn cross_built_executable_is_stripped_and_packed() {
        if skip_without(&["zstd", "aarch64-linux-gnu-gcc", "aarch64-linux-gnu-strip"]) {
            return;
        }
        let t = TempDir::new("pack-exe");
        let c = t.path().join("hello.c");
        std::fs::write(&c, "#include <stdio.h>\nint main(void){puts(\"hi\");return 0;}\n").unwrap();
        let exe = t.path().join("hello");
        assert!(sh(&format!("aarch64-linux-gnu-gcc -g -O2 -o {} {}", exe.display(), c.display())));
        let before = std::fs::read(&exe).unwrap();
        let p = pack(&exe, ArtifactKind::Exe, "hello", &t.path().join("out")).unwrap();
        // 元のファイルは書き換えない。strip したぶん小さい
        assert_eq!(std::fs::read(&exe).unwrap(), before);
        assert!(p.bytes < before.len() as u64);
        let back = t.path().join("back");
        assert!(sh(&format!("zstd -q -d -o {} {}", back.display(), p.zpath.display())));
        assert_eq!(sha256_file(&back).unwrap(), p.sha256);
        assert_eq!(std::fs::metadata(&back).unwrap().len(), p.bytes);
        check_aarch64_elf(&back).unwrap();
        assert!(!t.path().join("out/artifact.stripped").exists());
    }
}
