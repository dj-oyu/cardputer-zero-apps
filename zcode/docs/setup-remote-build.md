# 外でビルドする仕組みの導入手順（人向け）

zcode の「外でビルド・試験・型検査する仕組み」を使えるようにするために、**人が手でやること**の
手順書。仕組みの設計は `docs/remote-build.html`（なぜこの形か）と `docs/stage3-design.md`
（名前・約束の正本。§12 がこの手順の元）。

- 書いてある名前（`zcode-build`、`zcode-cz-1`、ポート 7411 など）は設計で決めたもの
- `<…>` は自分の環境の値に置き換える。**これらの値はリポジトリに書かない**（公開リポジトリ）。
  控えはパスワードマネージャーなどに
- 2026-11-17 時点。**プログラムの一部はまだ実装中**で、該当する手順には「実装待ち」と書いた

## 0. 全体の流れ

| 順 | やること | 要るもの | 所要の目安 |
|---|---|---|---|
| 1 | Cloudflare の準備（プラン・Zero Trust・トークン・Access） | Cloudflare のアカウントと、Cloudflare に載っているドメイン | 30 分 |
| 2 | Worker とビルド係の deploy | 手元の PC に Node と `docker` | 30 分〜（初回のイメージ作り） |
| 3 | PC をビルド係にする（任意） | Tailscale の管理画面、Windows の管理者権限 | 15 分 |
| 4 | 実機ごとの設定 | ssh で実機に入れること | 1 台 10 分 |

1 と 2 をやれば Cloudflare だけで動く。3 は「PC が点いているときは PC でビルドする」（速くて無料）
ための任意の手順。

## 1. Cloudflare の準備

### 1.1 プランとドメイン

- [ ] **Workers の有料プラン（Workers Paid、月 5 ドル）** にする。Containers は有料プランでしか
      使えない。料金の中身は `remote-build.html` §9（含まれる分を超えるのは、ビルド係を長く
      温めておいたとき）
- [ ] Cloudflare に載っているドメインを 1 つ決める（`<your-domain>`）。ビルド係の入口は
      **`<build-host>` = `zcode-build.<your-domain>`** にする

> ドメインを持っていない場合: `workers.dev` のホスト名でも動かせるが、設定が少し変わる
> （設計の未決事項。決めたら知らせてほしい）。

### 1.2 Zero Trust を有効にする

Cloudflare Access（入口の門番）を使うために要る。無料の枠で足りる。

- [ ] ダッシュボードの **Zero Trust** を開いて有効にする
- [ ] チーム名 **`<team-name>`** を控える（`<team-name>.cloudflareaccess.com` の部分）

### 1.3 サービストークンを実機ごとに作る

実機が「自分は登録済みの機体だ」と示すための鍵。**1 台に 1 つ**作る（失くした機体だけ止められる
ように）。

場所: Zero Trust → Access → Service Auth → **Service Tokens** → Create Service Token

| 実機 | トークンの名前 | 期限 |
|---|---|---|
| 1 台目 | `zcode-cz-1` | 1 年 |
| 2 台目 | `zcode-cz-2` | 1 年 |

- [ ] 作ると **Client ID**（`….access` で終わる）と **Client Secret** が出る。両方控える
- [ ] **Client Secret はこの画面で 1 回しか見えない**。閉じる前に必ず控える
- [ ] 期限の通知を有効にする（期限の 1 週間前にメールが来る）

### 1.4 Access のアプリケーションを作る

入口 `<build-host>` に、サービストークンを持つものだけを通す設定。

場所: Zero Trust → Access → Applications → Add an application → **Self-hosted**

- [ ] 名前 **`zcode-build`**、ドメイン **`<build-host>`**
- [ ] ポリシーを **1 つだけ** 作る:
  - 名前 `devices`
  - **Action = Service Auth**（ここが大事。Allow にすると人のログイン画面が出てしまい、実機の
    プログラムからは使えない。理由は `remote-build.html` §3）
  - Include = Service Token → 1.3 で作ったトークン（両方）
- [ ] **Allow のポリシーは作らない**（人のログインは通さない）
- [ ] 保存したら、アプリケーションの **AUD タグ**（64 桁の英数字）を控える

## 2. Worker とビルド係を deploy する

**実装待ち**: Worker（課題 C1・C2）とイメージ（課題 C3）ができてから。

### 2.1 手元の PC の準備

- [ ] **Node 22 以上**
- [ ] **`docker` コマンド**。wrangler がビルド係のイメージを作って Cloudflare に送るのに使う。
      いちばん手早いのは WSL の Ubuntu に Docker を入れる方法。開発コンテナで使っている
      `wslc` で代わりになるかは未確認

> deploy の方法は未決事項: (a) WSL の Ubuntu の Docker、(b) `wslc` を `WRANGLER_DOCKER_BIN`
> で使う（未確認）、(c) GitHub Actions から手動で deploy（Cloudflare の API トークンを
> リポジトリの secret に置く）。決めたら知らせてほしい。

### 2.2 deploy

```sh
cd zcode/remote/worker
npm ci
npx wrangler login          # ブラウザで Cloudflare にログインする
npx wrangler deploy --domain <build-host>
```

- 初回はイメージ（Rust・クロスコンパイラ・Node 入りで GB 級）を作って送るので時間がかかる
- `--domain` が使えない版の wrangler では、手元だけの設定ファイル `wrangler.local.jsonc` に
  `routes` を書いて `npx wrangler deploy -c wrangler.local.jsonc` とする。**このファイルは
  git に入れない**（`.gitignore` 済み）

### 2.3 秘密の値を入れる

聞かれたら値を貼る（画面にもファイルにも残らない）。

```sh
npx wrangler secret put ACCESS_TEAM_DOMAIN   # <team-name>.cloudflareaccess.com
npx wrangler secret put ACCESS_AUD           # 1.4 の AUD タグ
npx wrangler secret put DEVICES              # 下の形の JSON
```

`DEVICES` は「どの Client ID がどの機体か」の対応表:

```json
{"<zcode-cz-1 の Client ID>": "cz-1", "<zcode-cz-2 の Client ID>": "cz-2"}
```

### 2.4 確かめる

```sh
# ヘッダなし: 門番で止まる
curl -si https://<build-host>/v1/ws | head -1
#   → 302 か 403 なら正しい（Worker まで届いていない）

# トークンあり: Worker まで届き、「WebSocket ではない」と断られる
curl -si -H "CF-Access-Client-Id: <Client ID>" -H "CF-Access-Client-Secret: <Client Secret>" \
  https://<build-host>/v1/ws | head -1
#   → 426 か 400 なら正しい
```

- ヘッダなしで 200 や 426 が返ったら、**Access のアプリケーションが効いていない**（1.4 の
  ドメインの綴りを確かめる）。そのまま使わない
- ヘッダありで 302 が返ったら、ポリシーの Action が Service Auth になっていない

### 2.5 費用の通知

- [ ] Notifications → Billing → **Usage Based Billing** を追加する。含まれる分を超えたら
      メールが来る

## 3. PC をビルド係にする（任意）

PC が点いているときは PC でビルドする（無料、キャッシュが温かいので速い）。実機の設定が
`"backend": "auto"` なら、PC に届けば PC、届かなければ Cloudflare を使う。

**実装待ち**: `run-pc.ps1`（課題 C3）ができてから。

### 3.1 Tailscale の ACL

実機 → PC の 7411 番だけを通す。管理画面の Access controls に足す（タグは自分の環境のもの）:

```json
{ "action": "accept", "src": ["<実機のタグ>"], "dst": ["<PC のタグ>:7411"] }
```

grants の書き方を使っている場合は:

```json
{ "src": ["<実機のタグ>"], "dst": ["<PC のタグ>"], "ip": ["tcp:7411"] }
```

- 実機にタグが付いていなければ、先にタグを付ける（ACL の `tagOwners` と、機体の設定）
- PC の経路は TLS を使わない（Tailscale の中は WireGuard で暗号化済み）。**認証はこの ACL
  だけ**なので、7411 番を tailnet の外に開けないこと

### 3.2 PC でビルド係を動かす

```powershell
cd zcode
.\remote\container\run-pc.ps1          # 初回はイメージを作る。Ctrl+C で止める
.\remote\container\run-pc.ps1 -Build   # イメージを作り直す
```

### 3.3 Windows のファイアウォール

7411 番の受信を **Tailscale のインターフェースだけ**に許す（管理者の PowerShell）:

```powershell
New-NetFirewallRule -DisplayName "zc-buildd (tailnet only)" -Direction Inbound `
  -Protocol TCP -LocalPort 7411 -InterfaceAlias "Tailscale" -Action Allow
```

### 3.4 確かめる（実機から）

```sh
curl -s http://<PC の tailnet 上の名前>:7411/healthz    # → ok
```

## 4. 実機ごとの設定

### 4.1 実行環境を入れる

**実装待ち**: `zc-remote`（課題 D1〜D5）、インストーラ（課題 R1）ができてから。

実機ではダウンロードしない。PC で用意して scp で送り、インストーラが置く（sudo は要らない。
入る先は `~/.local/share/zcode/runtimes/` と `~/.local/bin`）。

```sh
# PC で用意するもの:
#   - LLRT（runtimes\llrt\build.ps1 で A53 向けに作ったもの）
#   - zc-remote（zcode/remote の build.sh で作ったもの）
#   - TypeScript 7 の tsc（npm の @typescript/typescript-linux-arm64 の tgz）と LLRT の型定義
#   どれも zcode/tools/runtimes.lock のチェックサムと合うこと

ssh <実機> 'mkdir -p /tmp/zc-rt'
scp <用意したファイル> zcode/tools/install-runtimes.sh zcode/tools/runtimes.lock <実機>:/tmp/zc-rt/
ssh <実機> 'cd /tmp/zc-rt && ./install-runtimes.sh --from . && rm -rf /tmp/zc-rt'
ssh <実機> '~/.local/bin/llrt --version; ~/.local/bin/tsc --version; ~/.local/bin/zc-remote --version'
```

- 実機の `/tmp` は RAM なので、送ったら早めに入れて消す（上の手順は最後に消している）
- チェックサムが合わなければインストーラは止まる。そのまま使わない

### 4.2 設定ファイルを書く

```sh
mkdir -p ~/.config/zcode && chmod 700 ~/.config/zcode
```

`~/.config/zcode/remote.json`（行き先と動き方）:

```json
{
  "backend": "auto",
  "cloudflare": { "url": "wss://<build-host>/v1/ws" },
  "pc": { "addr": "<PC の tailnet 上の名前>:7411" },
  "check_on_save": "always"
}
```

| 項目 | 値 |
|---|---|
| `backend` | `auto`（PC に届けば PC）/ `pc` / `cloudflare` |
| `check_on_save` | `always`（保存のたびに外で型検査）/ `warm`（接続が温かいときだけ。電池と通信を節約）/ `manual` |

PC をビルド係にしないなら `"backend": "cloudflare"` にして `pc` の行を消す。

`~/.config/zcode/remote-token.json`（**その実機の**サービストークン。1.3 で控えたもの）:

```json
{ "scheme": "cf-access-service-token", "client_id": "<…>.access", "client_secret": "<…>" }
```

```sh
chmod 600 ~/.config/zcode/remote-token.json
```

- **権限が 0600 より広いと、zc-remote は読まずに止まる**（ほかのユーザーに読まれないように）
- 2 台目には 2 台目のトークンを書く（同じトークンを使い回さない）

### 4.3 確かめる

```sh
zc-remote status            # つながるビルド係（PC / Cloudflare）が出る
mkdir -p /tmp/zc-hello/src && cd /tmp/zc-hello
printf '[package]\nname="hello"\nversion="0.1.0"\nedition="2021"\n' > Cargo.toml
printf 'fn main() { println!("hello from the builder"); }\n' > src/main.rs
zc-remote run src/main.rs   # 外でビルドして、実機で実行する
cd / && rm -rf /tmp/zc-hello
```

## 5. 困ったとき

| 症状 | 見るところ |
|---|---|
| `zc-remote` が「認証できない」 | トークンの Client ID / Secret の写し間違い、`DEVICES` にその Client ID があるか、トークンの期限 |
| 「時計が合っていない」と出る | 実機に時計の電池が無いので、起動直後は NTP が合うまで待つ（`/run/systemd/timesync/synchronized` ができれば合っている） |
| 「オフライン」とすぐ出る | Wi-Fi・テザリングがつながっているか |
| PC に届かない | 3.1 の ACL、3.3 のファイアウォール、`run-pc.ps1` が動いているか、3.4 の `/healthz` |
| 料金が気になる | Cloudflare の Containers の利用量。ビルド係を温めておく時間（既定 10 分）が長いとメモリの料金が増える |

## 6. トークンを失くした・漏れたとき、期限が来たとき

- **失くした・漏れた**: Zero Trust → Service Tokens でそのトークンを **Revoke** する。
  `DEVICES` からその Client ID の行を消して `wrangler secret put DEVICES` で入れ直す。
  その実機には新しいトークンを作って 4.2 をやり直す
- 漏れたトークンでできるのは「ビルド係の時間を使う」ことだけ（実機やほかのものには届かない）。
  それでも回数と 1 日の上限（1 台 1 日 400 ジョブ、設計の値）で抑えている
- **期限が近い**: **Refresh** で 1 年延びる（実機の設定は変えなくてよい）
- **秘密を入れ替えたい**: **Rotate**。古い秘密を並行して使える時間を選べるので、その間に
  実機の `remote-token.json` を書き換える

## 7. 後片付け（やめるとき）

- Cloudflare: `npx wrangler delete`（Worker と Durable Object）、Containers のイメージを消す、
  Access のアプリケーションとサービストークンを消す。不要なら Workers の有料プランを戻す
- PC: `run-pc.ps1` を止め、`wslc volume rm cz-buildd-data`、ファイアウォールの規則と ACL の
  行を消す
- 実機: `~/.config/zcode/remote*.json` と `~/.cache/zcode/bin/` を消す。実行環境は
  `install-runtimes.sh` の取り外しの手順で
