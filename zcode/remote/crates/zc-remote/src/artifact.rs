//! 成果物の受け取りと、SD の 1 つだけの控え（docs/stage3-design.md §7.7）。課題 D4。
//!
//! # 受け取り（RAM の tmpfs）
//!
//! 置き場は `$XDG_RUNTIME_DIR/zcode/bin/`（実機では `/run/user/1000/zcode/bin/`。上限
//! 35.3MB の tmpfs で noexec なし = 実測。控えの段 1 と共用）。
//!
//! 1. `.part-…` に、届いた zstd の流れを**展開しながら**書く（ヒープに全体を持たない。
//!    展開は pure Rust の ruzstd、窓は 2MiB まで）。同時に、圧縮されたままの流れも
//!    `.zin-…` に書く（SD の控え用。下）
//! 2. 終わったら、大きさと SHA-256 を確かめる。合わなければ消して失敗
//! 3. `chmod 0700`（JS は 0600）→ `rename` で `<名前>-<tree id の先頭 8 桁>` に置く。
//!    **途中のものは実行できる名前にしない**
//! 4. 同じプロジェクトの前の版を消す。全体が [`BIN_QUOTA`] を超えたら古い順に消す
//!
//! 途中で切れたら、圧縮後の位置（[`Receiver::received`]）から続きをもらう。
//!
//! ## tmpfs の中の名前（`<p>` = project id、`<s>` = SHA-256 の先頭 16 桁）
//!
//! | 名前 | 中身 |
//! |---|---|
//! | `<名前>-<tree 8 桁>` | 置いた成果物（0700 / JS は 0600） |
//! | `.cur-<p>` | そのプロジェクトの最新の成果物を指すシンボリックリンク（名前だけ。[`find_placed`] が読む） |
//! | `.zst-<p>` | 圧縮されたままの写し（下の `.zcb` と同じ形）。SD の控えを書くまでの間だけ |
//! | `.zin-<p>-<s>` | 受け取り中の圧縮の流れ（頭つき）。成功したら `.zst-<p>` に rename |
//! | `.part-<p>-<s>` | 受け取り中の展開後の中身（0600） |
//!
//! 途中のファイルの名前を `chan` ではなく SHA-256 で付けるのは、つなぎ直しで `chan` が
//! 変わっても続きから受け取れるようにするため。`begin` し直すと、残っていた `.zin-…` を
//! 読み直して `.part-…` を作り直す（展開の途中の状態は持ち越せないため。tmpfs なので速い）。
//! 読み直しで壊れていたら、0 からやり直す。
//!
//! [`Receiver`] を `finish` も `abort` もせずに捨てると、途中のファイルは残る（続きのため）。
//! 同じプロジェクトの別の成果物の `begin` で消える。
//!
//! # SD の控え（ユーザーの決定 ②）
//!
//! tmpfs は再起動で消えるので、**最後に成功した実行ファイルをプロジェクトごとに 1 つだけ**
//! `~/.cache/zcode/bin/<project id>.zcb` に残す（「再計算できるキャッシュは SD に書かない」の
//! 例外。ネットが無いと作り直せないため）。
//!
//! - 書く時機: 成功した build の [`LASTGOOD_DELAY`] 後（その間に次の build が成功したら
//!   そちらだけ書く）と、serve が終わるとき。**中身（SHA-256）が同じなら書かない**
//!   （時機を数えるのは serve。ここは [`LastGood::persist`] を呼ばれたら書くだけ）
//! - 書き方: 一時ファイル → fsync → rename → フォルダを fsync
//! - 形: 頭（マジック `ZCB1`・種類・名前・展開後の大きさと SHA-256・tree id・圧縮後の大きさ）
//!   + zstd の流れ（受け取ったものをそのまま。実機で圧縮し直さない）
//! - プロジェクトの数の上限 [`LASTGOOD_MAX`]。超えたら古い順（mtime）に消す
//! - ネットが無いときの build は、tmpfs に無ければここから展開して戻す
//!
//! `.zcb` の頭（リトルエンディアン）:
//!
//! ```text
//! "ZCB1"  u8 種類（1 = exe、2 = js）  u8 名前の長さ  名前
//! u64 展開後の大きさ  [32] SHA-256  u8 tree id の長さ  tree id  u64 圧縮後の大きさ
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ruzstd::decoding::FrameDecoder;
use sha2::{Digest, Sha256};
use zc_wire::limits::{CHUNK_BYTES, MAX_ARTIFACT_BYTES, ZSTD_WINDOW_LOG};
use zc_wire::msg::ArtifactKind;

/// tmpfs の `bin/` の合計の上限。
pub const BIN_QUOTA: u64 = 16 << 20;
/// 成功した build から SD に書くまでの待ち。
pub const LASTGOOD_DELAY: Duration = Duration::from_secs(60);
/// SD に残すプロジェクトの数の上限。
pub const LASTGOOD_MAX: usize = 8;

/// 成果物の説明（`artifact` メッセージから）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub project: String,
    /// tree id（置く名前の末尾に先頭 8 桁を使う）。
    pub tree: String,
    pub kind: ArtifactKind,
    /// 実行ファイルの名前（`foo`・`main.js`）。**パスの区切りを含むものは断る。**
    pub name: String,
    /// 展開後の SHA-256 と大きさ。
    pub sha256: String,
    pub bytes: u64,
    /// 圧縮後の大きさ。
    pub zbytes: u64,
}

/// 受け取りの失敗。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactError {
    /// 上限を超える・名前がおかしい・位置が飛んだ。
    Invalid(String),
    /// zstd として読めない・窓が大きすぎる。
    Decode(String),
    /// 大きさか SHA-256 が合わない。
    Mismatch,
    /// tmpfs がいっぱい（古い版を消しても入らない）。
    NoSpace,
    Io(String),
}

impl From<io::Error> for ArtifactError {
    fn from(e: io::Error) -> ArtifactError {
        ArtifactError::Io(e.to_string())
    }
}

/// 置いた成果物。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub path: PathBuf,
    pub kind: ArtifactKind,
}

/// 置き場（tmpfs と SD）。
#[derive(Clone, Debug)]
pub struct Dirs {
    /// `$XDG_RUNTIME_DIR/zcode/bin`
    pub run: PathBuf,
    /// `~/.cache/zcode/bin`（`XDG_CACHE_HOME` があればそちら）
    pub cache: PathBuf,
}

impl Dirs {
    /// 環境変数から決める。フォルダは無ければ作る（0700）。
    ///
    /// `XDG_RUNTIME_DIR` が無ければ `/run/user/<uid>`（あれば）。どちらも無ければ失敗
    /// （成果物を SD に置かないため、`/tmp` などには逃げない）。
    pub fn from_env() -> std::io::Result<Dirs> {
        let abs = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
        let runtime = abs("XDG_RUNTIME_DIR")
            .or_else(|| {
                // SAFETY: getuid は失敗しない。
                let p = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
                p.is_dir().then_some(p)
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR が無い"))?;
        let cache = abs("XDG_CACHE_HOME")
            .or_else(|| abs("HOME").map(|h| h.join(".cache")))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME が無い"))?;
        let dirs = Dirs { run: runtime.join("zcode/bin"), cache: cache.join("zcode/bin") };
        mkdir(&dirs.run)?;
        mkdir(&dirs.cache)?;
        Ok(dirs)
    }
}

/// 無ければ作る（0700）。あれば何も書かない。
fn mkdir(p: &Path) -> io::Result<()> {
    if p.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(p)
}

// ---- 名前と頭 ----

fn is_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_sha(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !is_hex(s) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn valid_project(p: &str) -> bool {
    !p.is_empty() && p.len() <= 64 && is_hex(p)
}

/// 置く名前にしてよいか（区切り・`.` で始まる名前・制御文字は断る）。
fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 200 && !n.starts_with('.') && !n.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

/// 圧縮後の大きさのゆとり（圧縮できない中身でも、zstd は 128KiB ごとに 3 バイト程度しか増えない）。
fn zbytes_ok(bytes: u64, zbytes: u64) -> bool {
    zbytes <= bytes + bytes / 1024 + 1024
}

/// 確かめて、展開後の SHA-256 を返す。
fn check_meta(m: &Meta) -> Result<[u8; 32], ArtifactError> {
    let bad = |s: &str| Err(ArtifactError::Invalid(s.to_owned()));
    if !valid_project(&m.project) {
        return bad("project id の形でない");
    }
    if m.tree.len() < 8 || m.tree.len() > 64 || !is_hex(&m.tree) {
        return bad("tree id の形でない");
    }
    if !valid_name(&m.name) {
        return Err(ArtifactError::Invalid(format!("名前に使えない: {:?}", m.name)));
    }
    if m.bytes > MAX_ARTIFACT_BYTES {
        return Err(ArtifactError::Invalid(format!("成果物が {}MiB を超えている", MAX_ARTIFACT_BYTES >> 20)));
    }
    if !zbytes_ok(m.bytes, m.zbytes) {
        return bad("圧縮後の大きさがおかしい");
    }
    parse_sha(&m.sha256).ok_or_else(|| ArtifactError::Invalid("SHA-256 の形でない".into()))
}

/// `.zcb` の頭。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    kind: ArtifactKind,
    name: String,
    bytes: u64,
    sha: [u8; 32],
    tree: String,
    zbytes: u64,
}

const MAGIC: &[u8; 4] = b"ZCB1";
/// 頭の最大の長さ。
const HDR_MAX: usize = 4 + 1 + 1 + 255 + 8 + 32 + 1 + 255 + 8;

impl Header {
    fn of(m: &Meta, sha: [u8; 32]) -> Header {
        Header { kind: m.kind, name: m.name.clone(), bytes: m.bytes, sha, tree: m.tree.clone(), zbytes: m.zbytes }
    }

    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(HDR_MAX);
        v.extend_from_slice(MAGIC);
        v.push(match self.kind {
            ArtifactKind::Exe => 1,
            ArtifactKind::Js => 2,
        });
        v.push(self.name.len() as u8);
        v.extend_from_slice(self.name.as_bytes());
        v.extend_from_slice(&self.bytes.to_le_bytes());
        v.extend_from_slice(&self.sha);
        v.push(self.tree.len() as u8);
        v.extend_from_slice(self.tree.as_bytes());
        v.extend_from_slice(&self.zbytes.to_le_bytes());
        v
    }

    /// 先頭から読む。（頭, 頭の長さ）。形がおかしければ `None`。
    fn decode(b: &[u8]) -> Option<(Header, usize)> {
        let mut r = b.strip_prefix(MAGIC)?;
        let mut take = |n: usize| -> Option<&[u8]> {
            let (a, rest) = (r.get(..n)?, r.get(n..)?);
            r = rest;
            Some(a)
        };
        let kind = match take(1)?[0] {
            1 => ArtifactKind::Exe,
            2 => ArtifactKind::Js,
            _ => return None,
        };
        let n = take(1)?[0] as usize;
        let name = std::str::from_utf8(take(n)?).ok()?.to_owned();
        let bytes = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let sha: [u8; 32] = take(32)?.try_into().ok()?;
        let n = take(1)?[0] as usize;
        let tree = std::str::from_utf8(take(n)?).ok()?.to_owned();
        let zbytes = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let len = b.len() - r.len();
        let ok = valid_name(&name)
            && tree.len() >= 8
            && is_hex(&tree)
            && bytes <= MAX_ARTIFACT_BYTES
            && zbytes_ok(bytes, zbytes);
        ok.then_some((Header { kind, name, bytes, sha, tree, zbytes }, len))
    }

    /// ファイルの先頭から読む。（頭, 頭の長さ, ファイルの長さ）。
    fn read(f: &mut File) -> io::Result<Option<(Header, usize, u64)>> {
        let len = f.metadata()?.len();
        let mut buf = [0u8; HDR_MAX];
        let mut n = 0;
        while n < HDR_MAX {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Header::decode(&buf[..n]).map(|(h, hl)| (h, hl, len)))
    }

    fn placed_name(&self) -> String {
        format!("{}-{}", self.name, &self.tree[..8])
    }
}

fn cur_link(run: &Path, project: &str) -> PathBuf {
    run.join(format!(".cur-{project}"))
}

fn zst_path(run: &Path, project: &str) -> PathBuf {
    run.join(format!(".zst-{project}"))
}

// ---- zstd の押し込み型の展開 ----

/// 届いた分だけ展開する（ruzstd の `decode_from_to` を、足りない分を持ち越しながら回す）。
/// 持つのは窓（2MiB まで）・ブロック 1 つ分の持ち越し・出口の 64KiB だけ。
struct Unzstd {
    dec: FrameDecoder,
    /// まだ使っていない圧縮の中身。
    pending: Vec<u8>,
    /// フレームの頭を読んだか（それまで `is_finished` は真を返すので使えない）。
    begun: bool,
    out: Box<[u8]>,
}

fn dec_err(e: impl core::fmt::Debug) -> ArtifactError {
    ArtifactError::Decode(format!("{e:?}"))
}

/// フレームの頭を確かめる。足りなければ `Ok(None)`、そろえば頭の長さ。
/// 窓が 2MiB を超えるもの・辞書つき・スキップ用のフレームは断る。展開後の大きさが頭に
/// あれば `expect` と比べる。
fn frame_header(b: &[u8], expect: u64) -> Result<Option<usize>, ArtifactError> {
    if b.len() < 5 {
        return Ok(None);
    }
    if b[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
        return Err(ArtifactError::Decode("zstd のフレームでない".into()));
    }
    let fhd = b[4];
    let fcs_flag = fhd >> 6;
    let single = fhd & 0x20 != 0;
    if fhd & 0x08 != 0 {
        return Err(ArtifactError::Decode("フレームの頭の予約ビット".into()));
    }
    if fhd & 0x03 != 0 {
        return Err(ArtifactError::Decode("辞書つきのフレーム".into()));
    }
    let fcs_len = match fcs_flag {
        0 => usize::from(single),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let hlen = 5 + usize::from(!single) + fcs_len;
    if b.len() < hlen {
        return Ok(None);
    }
    let fcs_at = 5 + usize::from(!single);
    let fcs = (fcs_len > 0).then(|| {
        let mut v = [0u8; 8];
        v[..fcs_len].copy_from_slice(&b[fcs_at..fcs_at + fcs_len]);
        let v = u64::from_le_bytes(v);
        if fcs_len == 2 {
            v + 256
        } else {
            v
        }
    });
    let window = if single {
        fcs.unwrap_or(0)
    } else {
        let wd = b[5];
        let base = 1u64 << (10 + (wd >> 3));
        base + (base >> 3) * u64::from(wd & 7)
    };
    if window > 1 << ZSTD_WINDOW_LOG {
        return Err(ArtifactError::Decode(format!("窓が大きすぎる（{window} バイト）")));
    }
    if fcs.is_some_and(|n| n != expect) {
        return Err(ArtifactError::Mismatch);
    }
    Ok(Some(hlen))
}

impl Unzstd {
    fn new() -> Unzstd {
        Unzstd { dec: FrameDecoder::new(), pending: Vec::new(), begun: false, out: vec![0; CHUNK_BYTES].into_boxed_slice() }
    }

    /// `data` を足して、展開できた分を `sink` に渡す。`last` なら流れの終わり（フレームが
    /// 終わっていなければ失敗）。
    fn feed(
        &mut self,
        data: &[u8],
        last: bool,
        expect: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), ArtifactError>,
    ) -> Result<(), ArtifactError> {
        self.pending.extend_from_slice(data);
        if !self.begun {
            match frame_header(&self.pending, expect)? {
                Some(_) => {}
                None if last => return Err(ArtifactError::Decode("途中で切れている".into())),
                None => return Ok(()),
            }
        }
        let mut pos = 0;
        loop {
            let avail = self.pending.len() - pos;
            if self.begun && self.dec.is_finished() {
                // フレームの終わり。残りを出し切る。
                loop {
                    let (_, w) = self.dec.decode_from_to(&[], &mut self.out).map_err(dec_err)?;
                    if w == 0 {
                        break;
                    }
                    sink(&self.out[..w])?;
                }
                if avail > 0 {
                    return Err(ArtifactError::Decode("フレームの後ろに余分なデータ".into()));
                }
                break;
            }
            // ruzstd は「最後のブロックの後のチェックサムが 4 バイトそろっていない」ときに
            // 4 バイト読んだと返す。終わりでない間は 4 バイト未満で呼ばない（ブロックの頭は
            // 3 バイトなので、それで進めなくなることは無い）。
            if !last && avail < 4 {
                break;
            }
            let (c, w) = self.dec.decode_from_to(&self.pending[pos..], &mut self.out).map_err(dec_err)?;
            self.begun = true;
            if c > avail {
                return Err(ArtifactError::Decode("途中で切れている".into()));
            }
            pos += c;
            if w > 0 {
                sink(&self.out[..w])?;
            }
            if c == 0 && w == 0 {
                break;
            }
        }
        self.pending.drain(..pos);
        if last && !(self.begun && self.dec.is_finished()) {
            return Err(ArtifactError::Decode("途中で切れている".into()));
        }
        Ok(())
    }
}

/// 展開後の中身の書き先（大きさの上限と SHA-256 を見ながら）。
struct Out {
    file: File,
    hash: Sha256,
    n: u64,
    limit: u64,
}

impl Out {
    fn put(&mut self, d: &[u8]) -> Result<(), ArtifactError> {
        self.n += d.len() as u64;
        if self.n > self.limit {
            return Err(ArtifactError::Mismatch);
        }
        self.hash.update(d);
        self.file.write_all(d)?;
        Ok(())
    }

    /// 大きさと SHA-256 が合うか。
    fn matches(&self, sha: &[u8; 32]) -> bool {
        self.n == self.limit && self.hash.clone().finalize().as_slice() == sha
    }
}

fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(true).mode(0o600).open(path)
}

/// `src` の zstd の流れ（`len` バイト）を全部展開して `out` へ。
fn decode_file(src: &mut File, len: u64, expect: u64, out: &mut Out) -> Result<(), ArtifactError> {
    let mut dec = Unzstd::new();
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut left = len;
    while left > 0 {
        let k = src.read(&mut buf[..(left.min(CHUNK_BYTES as u64) as usize)])?;
        if k == 0 {
            return Err(ArtifactError::Decode("途中で切れている".into()));
        }
        left -= k as u64;
        dec.feed(&buf[..k], false, expect, &mut |d| out.put(d))?;
    }
    dec.feed(&[], true, expect, &mut |d| out.put(d))
}

/// 置く: 権限を付けて `rename`、`.cur-<p>` を張り替え、前の版を消す。
fn place(run: &Path, project: &str, part: &Path, file: &File, h: &Header) -> Result<Placed, ArtifactError> {
    let mode = match h.kind {
        ArtifactKind::Exe => 0o700,
        ArtifactKind::Js => 0o600,
    };
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    let name = h.placed_name();
    let path = run.join(&name);
    fs::rename(part, &path)?;
    let link = cur_link(run, project);
    let old = fs::read_link(&link).ok();
    let tmp = run.join(format!(".cur-{project}.tmp"));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(&name, &tmp)?;
    fs::rename(&tmp, &link)?;
    if let Some(old) = old.and_then(|o| o.to_str().map(str::to_owned)) {
        if old != name && valid_name(&old) && !referenced(run, &old) {
            let _ = fs::remove_file(run.join(&old));
        }
    }
    Ok(Placed { path, kind: h.kind })
}

/// ほかのプロジェクトの `.cur-*` が `name` を指しているか。
fn referenced(run: &Path, name: &str) -> bool {
    let Ok(rd) = fs::read_dir(run) else { return false };
    rd.flatten().any(|e| {
        e.file_name().to_str().is_some_and(|n| n.starts_with(".cur-") && !n.ends_with(".tmp"))
            && fs::read_link(e.path()).is_ok_and(|t| t.as_os_str() == name)
    })
}

/// `need` バイトを足しても [`BIN_QUOTA`] に収まるように、古い順に消す。消してよいのは
/// 置いた成果物と `.zst-*`（`keep` は除く）。受け取り中のもの（`.zin-*`・`.part-*`）は
/// 数えるが消さない。
fn make_room(run: &Path, need: u64, keep: &[&Path]) -> Result<(), ArtifactError> {
    if need > BIN_QUOTA {
        return Err(ArtifactError::NoSpace);
    }
    let mut usage = 0u64;
    let mut cands: Vec<(i64, i64, PathBuf, u64)> = Vec::new();
    for e in fs::read_dir(run)?.flatten() {
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        let path = e.path();
        if keep.contains(&path.as_path()) {
            continue;
        }
        usage += m.len();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with('.') || name.starts_with(".zst-") {
            cands.push((m.mtime(), m.mtime_nsec(), path, m.len()));
        }
    }
    if usage + need <= BIN_QUOTA {
        return Ok(());
    }
    cands.sort();
    for (_, _, path, len) in cands {
        if usage + need <= BIN_QUOTA {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            usage -= len;
        }
    }
    // 指す先の無くなった `.cur-*` を片づける。
    if let Ok(rd) = fs::read_dir(run) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(".cur-") && fs::metadata(e.path()).is_err() {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    if usage + need > BIN_QUOTA {
        return Err(ArtifactError::NoSpace);
    }
    Ok(())
}

// ---- 受け取り ----

/// 残っていた `.zin-…` の頭が今回と同じなら、中身を展開し直す。（追記用に開いたファイル,
/// 受け取り済みのバイト数）。使えなければ `None`（`out` と `dec` は途中まで進んでいる）。
fn resume(zin_path: &Path, header: &Header, hdr_len: usize, out: &mut Out, dec: &mut Unzstd) -> Option<(File, u64)> {
    let mut f = OpenOptions::new().read(true).append(true).open(zin_path).ok()?;
    let (h, hl, len) = Header::read(&mut f).ok()??;
    let have = len - hl as u64;
    if h != *header || hl != hdr_len || have > header.zbytes {
        return None;
    }
    f.seek(SeekFrom::Start(hl as u64)).ok()?;
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut left = have;
    while left > 0 {
        let k = f.read(&mut buf[..(left.min(CHUNK_BYTES as u64) as usize)]).ok()?;
        if k == 0 {
            return None;
        }
        left -= k as u64;
        dec.feed(&buf[..k], false, header.bytes, &mut |d| out.put(d)).ok()?;
    }
    Some((f, have))
}

/// 成果物 1 つの受け取り。
pub struct Receiver {
    run: PathBuf,
    project: String,
    header: Header,
    #[allow(dead_code)]
    chan: u32,
    zin_path: PathBuf,
    part_path: PathBuf,
    /// 圧縮の流れの写し（頭つき。末尾に足していく）。
    zin: File,
    out: Out,
    dec: Unzstd,
    received: u64,
    /// 失敗した（途中のファイルは消してある）。
    dead: bool,
}

impl Receiver {
    /// 受け取りを始める。同じ `meta`（同じ SHA-256）の途中のファイルが残っていれば、
    /// そこから続ける（[`Receiver::received`] がその位置）。
    pub fn begin(dirs: &Dirs, meta: Meta, chan: u32) -> Result<Receiver, ArtifactError> {
        let sha = check_meta(&meta)?;
        let run = dirs.run.clone();
        mkdir(&run)?;
        let header = Header::of(&meta, sha);
        let hdr = header.encode();
        let tag = &meta.sha256[..16];
        let project = meta.project;
        let zin_path = run.join(format!(".zin-{project}-{tag}"));
        let part_path = run.join(format!(".part-{project}-{tag}"));

        // 同じプロジェクトの、別の成果物の途中のファイルは捨てる（受付は同じプロジェクトの
        // ジョブを直列にするので、同時に 2 つは来ない）。
        let mine = [format!(".zin-{project}-{tag}"), format!(".part-{project}-{tag}")];
        let (zp, pp) = (format!(".zin-{project}-"), format!(".part-{project}-"));
        for e in fs::read_dir(&run)?.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if (n.starts_with(&zp) || n.starts_with(&pp)) && !mine.iter().any(|m| *m == n) {
                let _ = fs::remove_file(e.path());
            }
        }

        let need = hdr.len() as u64 + meta.zbytes + meta.bytes;
        make_room(&run, need, &[&zin_path, &part_path])?;

        let mut out = Out { file: create_private(&part_path)?, hash: Sha256::new(), n: 0, limit: meta.bytes };
        let mut dec = Unzstd::new();

        // 続きから: 残っていた圧縮の流れを読み直して、展開後の中身を作り直す。
        let resumed = resume(&zin_path, &header, hdr.len(), &mut out, &mut dec);
        let (zin, received) = match resumed {
            Some(r) => r,
            None => {
                // 0 から。
                out.file.set_len(0)?;
                out.file.seek(SeekFrom::Start(0))?;
                out.hash = Sha256::new();
                out.n = 0;
                dec = Unzstd::new();
                let mut zin = create_private(&zin_path)?;
                zin.write_all(&hdr)?;
                (zin, 0)
            }
        };
        Ok(Receiver { run, project, header, chan, zin_path, part_path, zin, out, dec, received, dead: false })
    }

    /// 受け取り済みの、圧縮後のバイト数（`attach` の `artifact` に入れる）。
    pub fn received(&self) -> u64 {
        self.received
    }

    /// 圧縮された中身を足す。`offset` が [`Receiver::received`] と違えば
    /// [`ArtifactError::Invalid`]（手前なら重複として捨てる）。
    ///
    /// 展開できない・大きさを超えた、で失敗したら途中のファイルは消える（以後の `write` と
    /// `finish` も失敗する）。
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), ArtifactError> {
        if self.dead {
            return Err(ArtifactError::Invalid("失敗した受け取り".into()));
        }
        let end = offset + data.len() as u64;
        if end <= self.received {
            return Ok(());
        }
        if offset > self.received {
            return Err(ArtifactError::Invalid(format!("位置が飛んだ（{offset}、受け取り済み {}）", self.received)));
        }
        let data = &data[(self.received - offset) as usize..];
        if self.received + data.len() as u64 > self.header.zbytes {
            return Err(self.fail(ArtifactError::Invalid("圧縮後の大きさを超えた".into())));
        }
        let out = &mut self.out;
        let r = self
            .dec
            .feed(data, false, self.header.bytes, &mut |d| out.put(d))
            .and_then(|()| self.zin.write_all(data).map_err(ArtifactError::from));
        if let Err(e) = r {
            return Err(self.fail(e));
        }
        self.received += data.len() as u64;
        Ok(())
    }

    fn fail(&mut self, e: ArtifactError) -> ArtifactError {
        self.dead = true;
        let _ = fs::remove_file(&self.part_path);
        let _ = fs::remove_file(&self.zin_path);
        e
    }

    /// 終わり。確かめて、置く。
    ///
    /// 全部届いていなければ [`ArtifactError::Invalid`]（途中のファイルは残す。`begin` し直せば
    /// 続きから）。展開できない・大きさか SHA-256 が合わないときは、何も残さない。
    pub fn finish(mut self) -> Result<Placed, ArtifactError> {
        if self.dead {
            return Err(ArtifactError::Invalid("失敗した受け取り".into()));
        }
        if self.received != self.header.zbytes {
            return Err(ArtifactError::Invalid(format!("足りない（{} / {}）", self.received, self.header.zbytes)));
        }
        let out = &mut self.out;
        if let Err(e) = self.dec.feed(&[], true, self.header.bytes, &mut |d| out.put(d)) {
            return Err(self.fail(e));
        }
        if !self.out.matches(&self.header.sha) {
            return Err(self.fail(ArtifactError::Mismatch));
        }
        let r = (|| {
            let placed = place(&self.run, &self.project, &self.part_path, &self.out.file, &self.header)?;
            let zst = zst_path(&self.run, &self.project);
            fs::rename(&self.zin_path, &zst)?;
            // 前の版を消した後でも上限を超えていれば、古いものから消す（今の 2 つは残す）。
            let _ = make_room(&self.run, 0, &[&placed.path, &zst]);
            Ok(placed)
        })();
        if r.is_err() {
            let _ = fs::remove_file(&self.part_path);
            let _ = fs::remove_file(&self.zin_path);
        }
        r
    }

    /// やめる（途中のファイルを消す）。
    pub fn abort(mut self) {
        let _ = self.fail(ArtifactError::Mismatch);
    }
}

/// tmpfs にある、このプロジェクトの最新の成果物。
pub fn find_placed(dirs: &Dirs, project: &str) -> Option<Placed> {
    if !valid_project(project) {
        return None;
    }
    let target = fs::read_link(cur_link(&dirs.run, project)).ok()?;
    let name = target.to_str().filter(|n| valid_name(n))?;
    let path = dirs.run.join(name);
    let m = fs::symlink_metadata(&path).ok()?;
    if !m.is_file() {
        return None;
    }
    let kind = if m.mode() & 0o100 != 0 { ArtifactKind::Exe } else { ArtifactKind::Js };
    Some(Placed { path, kind })
}

/// SD の 1 つだけの控え。
pub struct LastGood;

impl LastGood {
    /// `project` の控えを SD に書く（tmpfs の `.zst-<project id>` から）。中身が同じなら
    /// 書かずに `Ok(false)`。書いたら tmpfs の圧縮された写しは消す。
    ///
    /// 写しが無ければ `Ok(false)`。写しが壊れていれば消して [`ArtifactError::Invalid`]。
    /// 中身が同じで書かなかったときも、写しは消す（役目が済んだので）。
    pub fn persist(dirs: &Dirs, project: &str) -> Result<bool, ArtifactError> {
        if !valid_project(project) {
            return Err(ArtifactError::Invalid("project id の形でない".into()));
        }
        let src_path = zst_path(&dirs.run, project);
        let mut src = match File::open(&src_path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let Some((h, _, _)) = Header::read(&mut src)?.filter(|(h, hl, len)| *len == *hl as u64 + h.zbytes) else {
            let _ = fs::remove_file(&src_path);
            return Err(ArtifactError::Invalid("tmpfs の写しが壊れている".into()));
        };
        let dst = dirs.cache.join(format!("{project}.zcb"));
        // 同じ中身なら書かない（頭だけ読む）。
        if let Ok(mut old) = File::open(&dst) {
            if let Ok(Some((oh, ohl, olen))) = Header::read(&mut old) {
                if oh.sha == h.sha && oh.bytes == h.bytes && olen == ohl as u64 + oh.zbytes {
                    drop(src);
                    let _ = fs::remove_file(&src_path);
                    return Ok(false);
                }
            }
        }
        mkdir(&dirs.cache)?;
        let tmp = dirs.cache.join(format!(".tmp-{project}"));
        let r = (|| -> io::Result<()> {
            let mut t = create_private(&tmp)?;
            src.seek(SeekFrom::Start(0))?;
            io::copy(&mut src, &mut t)?;
            t.sync_all()?;
            fs::rename(&tmp, &dst)?;
            File::open(&dirs.cache)?.sync_all()
        })();
        if let Err(e) = r {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        drop(src);
        let _ = fs::remove_file(&src_path);
        LastGood::prune(&dirs.cache)?;
        Ok(true)
    }

    /// SD の控えを tmpfs に展開して置く（ネットが無いときの build）。無ければ `Ok(None)`。
    /// 頭が壊れている・SHA-256 が合わないものは消して `Ok(None)`。
    pub fn restore(dirs: &Dirs, project: &str) -> Result<Option<Placed>, ArtifactError> {
        if !valid_project(project) {
            return Ok(None);
        }
        let path = dirs.cache.join(format!("{project}.zcb"));
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let broken = |path: &Path| -> Result<Option<Placed>, ArtifactError> {
            let _ = fs::remove_file(path);
            Ok(None)
        };
        let Some((h, hl, len)) = Header::read(&mut f)? else { return broken(&path) };
        if len != hl as u64 + h.zbytes {
            return broken(&path);
        }
        mkdir(&dirs.run)?;
        let part = dirs.run.join(format!(".part-{project}-restore"));
        make_room(&dirs.run, h.bytes, &[&part])?;
        let mut out = Out { file: create_private(&part)?, hash: Sha256::new(), n: 0, limit: h.bytes };
        f.seek(SeekFrom::Start(hl as u64))?;
        match decode_file(&mut f, h.zbytes, h.bytes, &mut out) {
            Ok(()) if out.matches(&h.sha) => {}
            Err(ArtifactError::Io(e)) => {
                let _ = fs::remove_file(&part);
                return Err(ArtifactError::Io(e));
            }
            _ => {
                let _ = fs::remove_file(&part);
                return broken(&path);
            }
        }
        match place(&dirs.run, project, &part, &out.file, &h) {
            Ok(p) => Ok(Some(p)),
            Err(e) => {
                let _ = fs::remove_file(&part);
                Err(e)
            }
        }
    }

    /// 控えのあるプロジェクトの数が [`LASTGOOD_MAX`] を超えていたら、古い順に消す。
    /// 書きかけの一時ファイル（`.tmp-*`。電源断の残り）も消す。
    pub fn prune(cache: &Path) -> std::io::Result<()> {
        let rd = match fs::read_dir(cache) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut zcb = Vec::new();
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.starts_with(".tmp-") {
                let _ = fs::remove_file(e.path());
            } else if n.ends_with(".zcb") {
                match e.metadata() {
                    Ok(m) if m.is_file() => zcb.push((m.mtime(), m.mtime_nsec(), e.path())),
                    _ => {}
                }
            }
        }
        if zcb.len() > LASTGOOD_MAX {
            zcb.sort();
            let extra = zcb.len() - LASTGOOD_MAX;
            for (_, _, p) in zcb.into_iter().take(extra) {
                fs::remove_file(p)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip_and_rejects() {
        let h = Header {
            kind: ArtifactKind::Js,
            name: "main.js".into(),
            bytes: 10,
            sha: [7; 32],
            tree: "ab".repeat(32),
            zbytes: 20,
        };
        let mut b = h.encode();
        b.extend_from_slice(b"rest");
        let (g, len) = Header::decode(&b).unwrap();
        assert_eq!(g, h);
        assert_eq!(len, b.len() - 4);
        assert!(Header::decode(&b[..len - 1]).is_none());
        let mut bad = b.clone();
        bad[0] = b'X';
        assert!(Header::decode(&bad).is_none());
        let mut bad = b.clone();
        bad[4] = 9;
        assert!(Header::decode(&bad).is_none());
    }

    #[test]
    fn frame_header_window() {
        // 単一セグメント・大きさ 1 バイト（FCS 1 バイト）。
        assert_eq!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x01], 1), Ok(Some(6)));
        assert_eq!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x01], 2), Err(ArtifactError::Mismatch));
        // 窓の記述子: 2^21 は通る、2^21 + 1/8 は断る。
        assert_eq!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, 11 << 3], 0), Ok(Some(6)));
        assert!(matches!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, (11 << 3) | 1], 0), Err(ArtifactError::Decode(_))));
        // 足りない。
        assert_eq!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd], 0), Ok(None));
        assert_eq!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd, 0xe0, 0, 0], 0), Ok(None));
        // 辞書・スキップ用のフレーム。
        assert!(frame_header(&[0x28, 0xb5, 0x2f, 0xfd, 0x21, 1, 1], 0).is_err());
        assert!(frame_header(&[0x50, 0x2a, 0x4d, 0x18, 0, 0], 0).is_err());
    }
}
