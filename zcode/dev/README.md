# 実機での開発用の起動口

SSH から zcode を直接起動すると、横で APPLaunch（ランチャー）が動き続けるので、
画面（上部のステータス、スクリーンセーバー）とキーボードを取り合う。正式に起動した
ときと同じ条件で試すため、ランチャーに「zcode (dev)」を 1 回だけ登録し、その中身
（実行ファイルと引数）は sudo なしで差し替えられるようにする。

## 仕組み

| 場所 | 中身 | 書き換え |
|---|---|---|
| `/usr/share/APPLaunch/applications/zcode-dev.desktop` | ランチャーへの登録。`Exec=/home/jimmy/.local/bin/zcode-dev` | 最初の 1 回だけ（root） |
| `~/.local/bin/zcode-dev` | 起動スクリプト（`zcode-dev.sh`） | jimmy |
| `~/.local/lib/zcode-dev/zcode` | 実行ファイル | jimmy |
| `~/.config/zcode-dev/args` | 引数（1 行目。無ければ `--keytest`） | jimmy |
| `~/.cache/zcode-dev/last.log` | 前回の標準エラー（`--stats` の結果など） | zcode |

- ランチャーの安全チェック（`cp0_desktop_exec_policy`）は「シェルの特殊記号が無い」
  「絶対パスなら実行可能なファイル」だけなので、ホームの下を指してよい
- ランチャーはユーザーのサービス（jimmy）として動き、起動したアプリも jimmy で動く。
  root 権限を渡す仕組みではない（sudo の設定は変えない）
- Esc を約 3 秒押し続けると、ランチャーがアプリを強制終了する（本体共通）

## 最初の 1 回（ユーザーが実機で行う）

```sh
sudo install -m 644 /tmp/zcode-dev.desktop /usr/share/APPLaunch/applications/zcode-dev.desktop
systemctl --user restart APPLaunch.service
```

## 差し替え（SSH から、sudo なし）

```sh
scp out/zcode <実機>:.local/lib/zcode-dev/zcode
ssh <実機> 'echo "--keytest --stats" > ~/.config/zcode-dev/args'
```

ユーザーがランチャーで「zcode (dev)」を選ぶと、その引数で起動する。結果は
`ssh <実機> cat ~/.cache/zcode-dev/last.log`。

## 片付け

```sh
sudo rm /usr/share/APPLaunch/applications/zcode-dev.desktop
systemctl --user restart APPLaunch.service
rm -rf ~/.local/bin/zcode-dev ~/.local/lib/zcode-dev ~/.config/zcode-dev ~/.cache/zcode-dev
```
