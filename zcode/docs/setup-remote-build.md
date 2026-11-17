# 外でビルドする仕組みの導入手順（人向け）

zcode の「外でビルド・試験・型検査する仕組み」を使えるようにするために、**人が手でやること**の
手順書。仕組みの設計は `docs/remote-build.html`（なぜこの形か）と `docs/stage3-design.md`
（名前・約束の正本。§12 がこの手順の元。食い違うときは、画面の操作についてはこの手順書が新しい）。

- 書いてある名前（`zcode-build`、`zcode-cz-1`、ポート 7411 など）は設計で決めたもの
- `<…>` は自分の環境の値に置き換える。**これらの値はリポジトリに書かない**（公開リポジトリ）。
  控えはパスワードマネージャーなどに
- 2026-11-17 時点。プログラムは全部できている（スナップショットのキャッシュ（課題 C4）だけ、最初の deploy の後）。
  まだ本物の Cloudflare では動かしていないので、deploy の後の確かめ（2.4）を必ずやる
- **Cloudflare の画面のメニューの名前はよく変わる。** 書いてある場所が見つからなければ、
  項目の名前（「Service Tokens」「Team name and domain」など）で Cloudflare の文書を検索する。
  メニューの名前は 2026-11-17 の画面（Zero Trust の左のメニュー: Overview / Insights & Logs /
  Team & Resources / **Access controls** / Traffic controls / … / Settings）に合わせた

## 0. 全体の流れ

| 順 | やること | 要るもの | 所要の目安 |
|---|---|---|---|
| 1 | Cloudflare の準備（プラン・Zero Trust・トークン） | Cloudflare のアカウント（独自ドメインは要らない。`workers.dev` を使う） | 20 分 |
| 2 | Worker とビルド係の deploy、Access を掛ける、秘密の値 | この Windows の PC（Node と WSL コンテナー `wslc`。docker は要らない） | 30 分〜（初回のイメージ作り） |
| 3 | PC をビルド係にする（任意） | Tailscale の管理画面、Windows の管理者権限 | 15 分 |
| 4 | 実機ごとの設定 | ssh で実機に入れること | 1 台 10 分 |

1 と 2 をやれば Cloudflare だけで動く。3 は「PC が点いているときは PC でビルドする」（速くて無料）
ための任意の手順。

## 1. Cloudflare の準備

### 1.1 プランと入口のホスト名

- [ ] **Workers の有料プラン（Workers Paid、月 5 ドル）** にする。Containers は有料プランでしか
      使えない。料金の中身は `remote-build.html` §9（含まれる分を超えるのは、ビルド係を長く
      温めておいたとき）
- [ ] 独自ドメインは使わない（2026-11-17 決定: 持っていない）。入口は Cloudflare が配る
      **`<build-host>` = `zcode-build.<subdomain>.workers.dev`**
  - `<subdomain>` はアカウントの workers.dev のサブドメイン（Workers & Pages の画面の右側、
    または初めて deploy したときに決める）

> **Access の掛け方に注意（2.3 で行う）。**
> - Worker の「Access」タブの「Protect this Worker behind Access」（Worker 単位の保護）は
>   **使わない**。Worker 単位の Access は WebSocket に対応しておらず、Upgrade が 403 で失敗すると
>   公式の文書に書かれている（[Cloudflare Access for Workers](https://developers.cloudflare.com/workers/configuration/cloudflare-access/)）。
>   アカウント全体の「すべての Worker を保護する」設定も同じ理由で入れない
> - 使うのは、deploy した後に **Zero Trust で self-hosted のアプリケーションを手で作り、公開ホスト名に
>   `zcode-build.<subdomain>.workers.dev` を入れる**方法（ホスト名に対する Access）。2026-11-17 に
>   この形で **WebSocket の握手（101）がサービストークン付きで通る**ことを確かめた
> - `<subdomain>` は**アカウントの本当の workers.dev のサブドメイン**でないと「domain does not belong to
>   zone」で断られる（2026-11-17、違う名前を入れて断られた）。本当の URL は
>   `cd zcode\remote\worker; npx wrangler triggers deploy` の最後の行（コードもイメージも送り直さない）か、
>   Worker の **Domains** タブで分かる
> - Worker の **Domains** タブの「Enable Access →」は、Worker 単位の保護（上の「Protect this Worker」）に
>   つながる可能性があるので使わない

### 1.2 Zero Trust を有効にする

Cloudflare Access（入口の門番）を使うために要る。無料の枠で足りる。

- [ ] ダッシュボードの **Zero Trust** を開いて有効にする（初回はチーム名を決める画面が出る）
- [ ] チーム名 **`<team-name>`** を控える（`<team-name>.cloudflareaccess.com` の部分）。
      後から見るときは **Zero Trust → Settings → Team name and domain**

### 1.3 サービストークンを実機ごとに作る

実機が「自分は登録済みの機体だ」と示すための鍵。**1 台に 1 つ**作る（失くした機体だけ止められる
ように）。

場所: **Zero Trust → Access controls → Service credentials → Service Tokens → Create Service Token**

| 実機 | トークンの名前 | 期限 |
|---|---|---|
| 1 台目 | `zcode-cz-1` | 1 年 |
| 2 台目 | `zcode-cz-2` | 1 年 |

- [ ] 作ると **Client ID**（`….access` で終わる）と **Client Secret** が出る。両方控える
- [ ] **Client Secret はこの画面で 1 回しか見えない**。閉じる前に必ず控える
- [ ] 期限の通知を有効にする（期限の 1 週間前にメールが来る）
- タグ（例 `app: zcode-build`）を付けておくと、2.3 のポリシーで「タグの付いたトークン」として
  まとめて指定できる（任意。付けなければトークンを 1 つずつ選ぶ）

## 2. Worker とビルド係を deploy する

この Windows の PC から deploy する（2026-11-17 決定）。**docker は入れない**: wrangler が
イメージを作って送るのに呼ぶ docker を、`remote\container\deploy.ps1` が WSL コンテナー（`wslc`）に
読み替えて渡す（中身は `remote/container/wrangler-docker/`）。

### 2.1 手元の PC の準備

- [ ] **Node 22.13 以上**（`node --version`）
- [ ] **`wslc`**（WSL 2.9.3 以降。開発コンテナで使っているもの。`wslc version`）
- [ ] wrangler にログインする（ブラウザが開く。初回だけ）:
  ```powershell
  cd zcode\remote\worker
  npm ci                      # 依存を入れる（インストールスクリプトは .npmrc で止めてある）
  npx wrangler login
  ```

### 2.2 deploy

```powershell
cd zcode
.\remote\container\deploy.ps1 -DryRun   # 先に試す: イメージを wslc で作り、Worker を束ねるだけ（送らない）
.\remote\container\deploy.ps1           # 本番: イメージを Cloudflare のレジストリへ送り、Worker を出す
```

- 初回はイメージ（Rust・クロスコンパイラ・Node 入りで約 1.5GB）を作って送るので時間がかかる。
  2 回目からは変わった層だけ
- 出る先は `https://zcode-build.<subdomain>.workers.dev`。設定（`wrangler.jsonc`）は
  `workers_dev: true`、**`preview_urls: false`**（版ごとのプレビュー URL は出さない）
- `-DryRun` は Cloudflare にはつながない（2026-11-17 に確かめた）。本番の deploy で初めて試すことに
  なるのは、Cloudflare のレジストリ（`registry.cloudflare.com`）への login と push、その後の
  イメージの準備。そこで失敗したら、出た文言を控えて知らせてほしい
- deploy の直後は、まだ Access も秘密の値も無い。この間に届いた依頼は、Worker が
  「設定が無い」（500）で断る（Access の JWT が無ければ通さない作り）。続けて 2.3・2.4 をやる
- 作ったイメージは `wslc` の中に残る（`wslc images` の `zcode-build-buildsession-builder`）。
  要らなくなったら `wslc rmi <名前>`

### 2.3 Access を掛ける（workers.dev のホスト名）

入口 `<build-host>` に、サービストークンを持つものだけを通す設定。deploy の後でないとできない。

1. [ ] 本当の URL を確かめる: `cd zcode\remote\worker; npx wrangler triggers deploy` の最後の行
       （`https://zcode-build.<subdomain>.workers.dev`）。Worker の **Domains** タブでも見える
2. [ ] Zero Trust → **Access controls → Applications** → **Create new application** →
       **Self-hosted and private** → **Add public hostname** に、1 のホスト名
       `zcode-build.<subdomain>.workers.dev` をそのまま入れる（ドメインの一覧には出ないので文字で入れる）。
       「Add Workers」（Worker を指定する形）は使わない
3. [ ] ポリシーを 1 つだけ: **Action = Service Auth**、Include = **Service Token** → 1.3 の
       トークン（両方）。**Allow のポリシーは作らない**（人のログインは通さない。Allow にすると人の
       ログイン画面が出てしまい、実機のプログラムからは使えない。理由は `remote-build.html` §3）
4. [ ] アプリケーションの **AUD タグ**（Application Audience、64 桁の英数字）を控える
       （アプリケーションの設定の概要・基本情報の欄）
5. [ ] トークン無しで門番に止められるか: `curl.exe -s -o NUL -w "%{http_code}" https://<build-host>/v1/ws`
       が **403**（Access が無いと Worker まで届いて 426 になる）

- Worker の **Access** タブの「Protect this Worker」と、**Domains** タブの「Enable Access →」は
  使わない（Worker 単位の保護は WebSocket が 403 になる）

### 2.4 秘密の値を入れる

聞かれたら値を貼る（画面にもファイルにも残らない）。

```powershell
cd zcode\remote\worker
npx wrangler secret put ACCESS_TEAM_DOMAIN   # <team-name>.cloudflareaccess.com
npx wrangler secret put ACCESS_AUD           # 2.3 の AUD タグ
npx wrangler secret put DEVICES              # 下の形の JSON
```

`DEVICES` は「どの Client ID がどの機体か」の対応表:

```json
{"<zcode-cz-1 の Client ID>": "cz-1", "<zcode-cz-2 の Client ID>": "cz-2"}
```

### 2.5 確かめる

まず **WebSocket が workers.dev の Access を通るか**（いちばん先に確かめること。通らなければ
1.1 の注意のとおり、安いドメインに切り替える）:

```sh
# ヘッダなし: 門番で止まる
curl -si https://<build-host>/v1/ws | head -1
#   → 302 か 403 なら正しい（Worker まで届いていない）

# トークンあり・Upgrade なし: Worker まで届き、「WebSocket ではない」と断られる
curl -si -H "CF-Access-Client-Id: <Client ID>" -H "CF-Access-Client-Secret: <Client Secret>" \
  https://<build-host>/v1/ws | head -1
#   → 426 なら正しい

# トークンあり・Upgrade あり: WebSocket の握手が通る
curl -si --http1.1 -H "CF-Access-Client-Id: <Client ID>" -H "CF-Access-Client-Secret: <Client Secret>" \
  -H "Connection: Upgrade" -H "Upgrade: websocket" -H "Sec-WebSocket-Version: 13" \
  -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" -H "Sec-WebSocket-Protocol: zc-build.v1" \
  --max-time 60 https://<build-host>/v1/ws | head -1
#   → 101 なら通っている（初回はコンテナの起動を待つので数十秒かかることがある。Ctrl+C で切ってよい）
#   → 403 なら Access が WebSocket を止めている。Worker の「Protect this Worker」が入っていないか、
#      2.3 のアプリケーションが workers.dev のホスト名のものかを見る。それでもだめなら独自ドメインへ
```

- ヘッダなしで 200 や 426 が返ったら、**Access が効いていない**（2.3 の 1 をやり直す）。
  そのまま使わない
- ヘッダありで 302 が返ったら、ポリシーの Action が Service Auth になっていない
- 101 まで通ったら、実機から `zc-remote status`（4.3）

### 2.6 費用（月の上限と通知）

**Worker が自分で月の費用を抑える**（2026-11-17 決定: 月の超過は 5 ドルまで。Workers Paid の
5 ドルと合わせて月 10 ドル前後）。設定は `remote/worker/wrangler.jsonc` の `vars` にまとめてある:

| 値 | 既定 | 意味 |
|---|---|---|
| `INSTANCE_TYPE` | `""`（空） | 空なら下の 3 つの custom。名前つき（`lite` / `standard-1`〜`standard-4`）も書ける |
| `INSTANCE_VCPU`・`INSTANCE_MEMORY_MIB`・`INSTANCE_DISK_MB` | `1`・`3072`・`12000` | コンテナの大きさ。メモリは 1 vCPU あたり 3072MiB 以上（Cloudflare の決まり）。外れた値は既定に戻る（ログに 1 行） |
| `KEEP_WARM_S` | `300` | 最後のジョブ・実機のメッセージから、コンテナを起こしておく秒数。過ぎたら DO が止める |
| `BUDGET_OVERAGE_USD` | `5.00` | **月の超過（含まれる分を超えた料金）の上限**（ドル、アカウント全体で 1 つ） |

- 上限に近づくと（今の超過 + 次の 1 ジョブ 15 分と keep-warm の見込み > 上限）、新しいジョブを
  断り、止まっているコンテナは起こさない。走っているジョブは最後まで終わり、その後すぐコンテナを
  止める。UTC の月の 1 日 0 時に戻る
- 実機では、`zc-remote run` などが「上限: 今月のビルド係の費用が上限に達した（超過 x.xx ドル・上限
  5.00 ドル）。2026-12-01 0 時（UTC）に戻る」と出して終了コード 5 で終わる。zcode の保存時の check は
  黙って何もしない（波線はそのまま）
- 数えているのは料金表からの見込み（メモリ・ディスクは動いている時間 × 大きさ、CPU は「ジョブが
  走っていた時間 × vCPU」で多めに見積もる）。**請求額そのものではない**
- 変えるとき: `wrangler.jsonc` の値を書き換えて deploy し直す（2.2）。ダッシュボードで vars を
  変えても、次の deploy で `wrangler.jsonc` の値に戻る。上限を上げれば、その月の残りもすぐ使える。
  大きさの変更は、動いているコンテナが止まって次に起こすときから効く
- 今月の使用量の見込みは `GET /v1/usage`（Access の内側。トークンが要る）:
  ```sh
  curl -s -H "CF-Access-Client-Id: <Client ID>" -H "CF-Access-Client-Secret: <Client Secret>" \
    https://<build-host>/v1/usage
  ```
  返事は JSON: `month`・`resets_at`・`budget_usd`・`exhausted`・`usage`（`memory_gib_h`・
  `vcpu_min_est`・`disk_gb_h`）・`included`・`overage_usd`（`memory`・`cpu`・`disk`・`total`）・
  `devices`（実機ごと）。`zc-remote status` はまだこれを表示しない
- 目安（1 vCPU・3GiB・12GB、料金表からの計算）: 動いている 1 時間でメモリ 0.027 ドル・ディスク
  0.003 ドル、ジョブが走っている 1 時間で CPU 0.072 ドル。含まれる分（メモリ 25GiB 時間 = 約 8 時間）の
  後、上限の 5 ドルまでで動かせるのは、メモリだけなら約 190 時間、ディスク込みで約 175 時間、
  ジョブが 1 割の時間走るなら約 150 時間

**Cloudflare 側でも 2 本目の守りを入れておく**（Worker の数え方が外れたときのため）:

- [ ] **予算の通知**（金額で決める）: **Manage Account → Billing → Billable Usage → Create budget
      alert**（または **Notifications → Add → Budget Alert**）。**8 ドル前後**にしておくと、Worker の
      上限（プラン 5 ドル + 超過 5 ドル）の手前で知らせが来る。2026 年の途中から、有料の従量課金の
      アカウントには 10 ドルの通知が自動で入っていることがある
- [ ] 必要なら、製品ごとの使用量の通知: **Notifications → Add → Usage Based Billing**
      （製品に Containers を選ぶ）
- Workers と Durable Objects には **使用額の上限**（超えたら止める）も設定できる
  （Billing → spending limits。止まるとビルドもできなくなるので、入れるかは好みで）

## 3. PC をビルド係にする（任意）

PC が点いているときは PC でビルドする（無料、キャッシュが温かいので速い）。実機の設定が
`"backend": "auto"` なら、PC に届けば PC、届かなければ Cloudflare を使う。

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
.\remote\container\run-pc.ps1          # 初回はイメージを作る（数分）。Ctrl+C で止める
.\remote\container\run-pc.ps1 -Build   # イメージを作り直す（zc-buildd や Dockerfile を変えたとき）
.\remote\container\run-pc.ps1 -Detach  # 裏で動かす。止めるのは -Stop
```

- 待つのは **Tailscale のインターフェースの IPv4 アドレスの 7411 番だけ**（スクリプトが調べて
  `wslc run -p <そのアドレス>:7411:7411` にする）。Tailscale が動いていなければ起動しない。
  手元だけで試すときは `-Bind 127.0.0.1`
- crate・npm のキャッシュと cargo の出力は名前付きボリューム `cz-buildd-data` に残る
- Tailscale を止めた・つなぎ直したときは、スクリプトを動かし直す（待ち受けがどうなるかは未確認）

### 3.3 Windows のファイアウォール

7411 番の受信を **Tailscale のインターフェースだけ**に許す（管理者の PowerShell）:

```powershell
New-NetFirewallRule -DisplayName "zc-buildd (tailnet only)" -Direction Inbound `
  -Protocol TCP -LocalPort 7411 -InterfaceAlias "Tailscale" -Action Allow
```

- Windows の側で 7411 番を待っているのは `dllhost.exe`（wslc のサービス）。規則はプログラムでは
  なく、ポートとインターフェースで書く（上のとおり）

### 3.4 確かめる（実機から）

```sh
curl -s http://<PC の tailnet 上の名前>:7411/healthz    # → ok
```

## 4. 実機ごとの設定

### 4.1 実行環境を入れる


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
| `check_on_save` | `always`（既定。保存のたびに外で型検査）/ `idle`（保存のたびに加えて、打鍵が 1.2 秒止まったら未保存の中身で型検査。SD には書かない）/ `warm`（接続が温かいときだけ。電池と通信を節約）/ `manual` |

PC をビルド係にしないなら `"backend": "cloudflare"` にして `pc` の行を消す。

`idle` の打鍵が止まったときの型検査は、届かない・上限などで失敗しても何も知らせない（波線と
`⊗ n` だけが変わる）。失敗したら次の保存まで休む。設定を書き換えたら、次の保存から効く
（動いている serve も読み直す）。

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
| 料金が気になる | 2.6 の `/v1/usage`。ビルド係を温めておく時間（`KEEP_WARM_S`、既定 5 分）とコンテナの大きさがメモリの料金を決める |
| 「上限: 今月のビルド係の費用が上限に達した」 | 2.6 の月の上限。`/v1/usage` で見込みを見る。上げるなら `BUDGET_OVERAGE_USD` を変えて deploy |

## 6. トークンを失くした・漏れたとき、期限が来たとき

- **失くした・漏れた**: **Zero Trust → Access controls → Service credentials → Service Tokens** で
  そのトークンを **Revoke** する。
  `DEVICES` からその Client ID の行を消して `wrangler secret put DEVICES` で入れ直す。
  その実機には新しいトークンを作って 4.2 をやり直す
- 漏れたトークンでできるのは「ビルド係の時間を使う」ことだけ（実機やほかのものには届かない）。
  それでも回数と 1 日の上限（1 台 1 日 400 ジョブ、設計の値。check は数えない）と、月の費用の上限
  （2.6、超過 5 ドル）で抑えている
- **期限が近い**: **Refresh** で 1 年延びる（実機の設定は変えなくてよい）
- **秘密を入れ替えたい**: **Rotate**。古い秘密を並行して使える時間を選べるので、その間に
  実機の `remote-token.json` を書き換える

## 7. 後片付け（やめるとき）

- Cloudflare: `npx wrangler delete`（Worker と Durable Object）、Containers のイメージを消す、
  Access のアプリケーション（**Access controls → Applications**）と再利用のポリシー
  （**Access controls → Policies**）とサービストークン（**Access controls → Service credentials**）を
  消す。不要なら Workers の有料プランを戻す
- PC: `run-pc.ps1 -Stop`、`wslc volume remove cz-buildd-data`、`wslc rmi cz-builder:latest`
  （と deploy で作った `zcode-build-buildsession-builder` のイメージ）、ファイアウォールの規則と
  ACL の行を消す
- 実機: `~/.config/zcode/remote*.json` と `~/.cache/zcode/bin/` を消す。実行環境は
  `install-runtimes.sh` の取り外しの手順で
