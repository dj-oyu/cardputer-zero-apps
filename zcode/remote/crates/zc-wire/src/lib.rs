//! 実機（`zc-remote`）⇔ ビルド係（`zc-buildd`）の約束（docs/stage3-design.md §5）。
//!
//! Cloudflare 経由でも PC 直結でも同じ。WebSocket 1 本に全部流す:
//! 制御は JSON のテキストフレーム（[`msg`]）、ファイルの中身と成果物はバイナリフレーム
//! （16 バイトの頭つき。[`msg::BinHeader`]）。base64 にしない。
//!
//! | モジュール | 中身 | 課題 |
//! |---|---|---|
//! | [`msg`] | メッセージの型（serde）と、バイナリフレームの頭 | W2 |
//! | [`tree`] | 目録（パス・大きさ・SHA-256）、tree id、job id | W2 |
//! | [`ws`] | WebSocket の枠（RFC 6455。クライアント・サーバ両方） | W1 |
//! | [`http`] | Upgrade の依頼と返事（HTTP/1.1） | W1 |
//!
//! この crate は I/O を持たない（バイト列 ⇔ 型）。実機側とビルド係の両方が使うので、
//! 依存は pure Rust だけ（serde・serde_json・sha2）。

pub mod http;
pub mod msg;
pub mod tree;
pub mod ws;

/// 約束の版（`hello` の `v`）。合わなければ `error {code:"version"}` を返して切る。
pub const PROTOCOL_VERSION: u32 = 1;
/// WebSocket の口。
pub const WS_PATH: &str = "/v1/ws";
/// `Sec-WebSocket-Protocol`。
pub const SUBPROTOCOL: &str = "zc-build.v1";
/// ビルド係の受付のポート（コンテナの中でも PC でも同じ）。
pub const DEFAULT_PORT: u16 = 7411;

/// 大きさの上限（docs/stage3-design.md §11.2）。受付は超えたものを断る。実機側は送る前に断る。
pub mod limits {
    /// 1 ファイル。
    pub const MAX_FILE_BYTES: u64 = 4 << 20;
    /// 1 つの木の合計。
    pub const MAX_TREE_BYTES: u64 = 32 << 20;
    /// 1 つの木のファイル数。
    pub const MAX_TREE_FILES: usize = 2000;
    /// テキストフレーム 1 つ（JSON）。
    pub const MAX_TEXT_FRAME: usize = 1 << 20;
    /// バイナリフレーム 1 つの中身（頭を除く）。
    pub const CHUNK_BYTES: usize = 64 << 10;
    /// 成果物（展開後）。実機の tmpfs（35.3MB、控えと共用）に置くので小さく保つ。
    pub const MAX_ARTIFACT_BYTES: u64 = 12 << 20;
    /// 1 ジョブのログ（受付が残す量。超えたら先頭を捨てて「省略」の印）。
    pub const MAX_LOG_BYTES: usize = 1 << 20;
    /// 1 ジョブの診断の件数（超えたら切って `truncated`）。
    pub const MAX_DIAGNOSTICS: usize = 500;
    /// 書き戻すファイル（Cargo.lock など）。
    pub const MAX_WRITEBACK_BYTES: usize = 512 << 10;
    /// zstd の窓（実機の展開メモリを抑える）。2MiB。
    pub const ZSTD_WINDOW_LOG: u32 = 21;
}
