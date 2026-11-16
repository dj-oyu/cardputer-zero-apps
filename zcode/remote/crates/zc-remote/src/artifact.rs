//! 成果物の受け取りと、SD の 1 つだけの控え（docs/stage3-design.md §7.7）。課題 D4。
//!
//! # 受け取り（RAM の tmpfs）
//!
//! 置き場は `$XDG_RUNTIME_DIR/zcode/bin/`（実機では `/run/user/1000/zcode/bin/`。上限
//! 35.3MB の tmpfs で noexec なし = 実測。控えの段 1 と共用）。
//!
//! 1. `.part-<chan>` に、届いた zstd の流れを**展開しながら**書く（ヒープに全体を持たない。
//!    展開は pure Rust の ruzstd、窓は 2MiB まで）。同時に、圧縮されたままの流れも
//!    `.zst-<project id>` に書く（SD の控え用。下）
//! 2. 終わったら、大きさと SHA-256 を確かめる。合わなければ消して失敗
//! 3. `chmod 0700`（JS は 0600）→ `rename` で `<名前>-<tree id の先頭 8 桁>` に置く。
//!    **途中のものは実行できる名前にしない**
//! 4. 同じプロジェクトの前の版を消す。全体が [`BIN_QUOTA`] を超えたら古い順に消す
//!
//! 途中で切れたら、圧縮後の位置（[`Receiver::received`]）から続きをもらう。
//!
//! # SD の控え（ユーザーの決定 ②）
//!
//! tmpfs は再起動で消えるので、**最後に成功した実行ファイルをプロジェクトごとに 1 つだけ**
//! `~/.cache/zcode/bin/<project id>.zcb` に残す（「再計算できるキャッシュは SD に書かない」の
//! 例外。ネットが無いと作り直せないため）。
//!
//! - 書く時機: 成功した build の [`LASTGOOD_DELAY`] 後（その間に次の build が成功したら
//!   そちらだけ書く）と、serve が終わるとき。**中身（SHA-256）が同じなら書かない**
//! - 書き方: 一時ファイル → fsync → rename → フォルダを fsync
//! - 形: 頭（マジック `ZCB1`・種類・名前・展開後の大きさと SHA-256・tree id）+ zstd の流れ
//!   （受け取ったものをそのまま。実機で圧縮し直さない）
//! - プロジェクトの数の上限 [`LASTGOOD_MAX`]。超えたら古い順に消す
//! - ネットが無いときの build は、tmpfs に無ければここから展開して戻す

use std::path::{Path, PathBuf};
use std::time::Duration;
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
    pub fn from_env() -> std::io::Result<Dirs> {
        todo!("D4")
    }
}

/// 成果物 1 つの受け取り。
pub struct Receiver {
    _private: (),
}

impl Receiver {
    /// 受け取りを始める。同じ `meta`（同じ SHA-256）の途中のファイルが残っていれば、
    /// そこから続ける（[`Receiver::received`] がその位置）。
    pub fn begin(dirs: &Dirs, meta: Meta, chan: u32) -> Result<Receiver, ArtifactError> {
        let _ = (dirs, meta, chan);
        todo!("D4")
    }

    /// 受け取り済みの、圧縮後のバイト数（`attach` の `artifact` に入れる）。
    pub fn received(&self) -> u64 {
        todo!("D4")
    }

    /// 圧縮された中身を足す。`offset` が [`Receiver::received`] と違えば
    /// [`ArtifactError::Invalid`]（手前なら重複として捨てる）。
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), ArtifactError> {
        let _ = (offset, data);
        todo!("D4")
    }

    /// 終わり。確かめて、置く。
    pub fn finish(self) -> Result<Placed, ArtifactError> {
        todo!("D4")
    }

    /// やめる（途中のファイルを消す）。
    pub fn abort(self) {
        todo!("D4")
    }
}

/// tmpfs にある、このプロジェクトの最新の成果物。
pub fn find_placed(dirs: &Dirs, project: &str) -> Option<Placed> {
    let _ = (dirs, project);
    todo!("D4")
}

/// SD の 1 つだけの控え。
pub struct LastGood;

impl LastGood {
    /// `project` の控えを SD に書く（tmpfs の `.zst-<project id>` から）。中身が同じなら
    /// 書かずに `Ok(false)`。書いたら tmpfs の圧縮された写しは消す。
    pub fn persist(dirs: &Dirs, project: &str) -> Result<bool, ArtifactError> {
        let _ = (dirs, project);
        todo!("D4")
    }

    /// SD の控えを tmpfs に展開して置く（ネットが無いときの build）。無ければ `Ok(None)`。
    /// 頭が壊れている・SHA-256 が合わないものは消して `Ok(None)`。
    pub fn restore(dirs: &Dirs, project: &str) -> Result<Option<Placed>, ArtifactError> {
        let _ = (dirs, project);
        todo!("D4")
    }

    /// 控えのあるプロジェクトの数が [`LASTGOOD_MAX`] を超えていたら、古い順に消す。
    pub fn prune(cache: &Path) -> std::io::Result<()> {
        let _ = cache;
        todo!("D4")
    }
}

// 受け入れ試験（課題 D4）は tests/artifact.rs に（置き場は試験ごとの一時フォルダ）:
// 1 バイトずつ・64KB ずつの書き込みで同じ結果、SHA-256 の食い違いで何も残らない、途中で
// やめて begin し直すと続きから、置いた後は実行権限つき・途中は実行できる名前が無い、
// 名前に `/` や `..`、上限超え、窓が 2MiB を超える流れ、BIN_QUOTA で古い版が消える、
// persist → tmpfs を消す → restore で同じ中身、同じ中身の persist は書かない（mtime が
// 変わらない）、壊れた控え。正解の zstd の流れは `zstd` コマンドで作って tests/data に置く。
