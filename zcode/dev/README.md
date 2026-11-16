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
| `/usr/share/APPLaunch/share/images/zcode-dev.png` | 下のアイコンへのシンボリックリンク | 最初の 1 回だけ（root） |
| `~/.local/lib/zcode-dev/icon.png` | アイコン（`tools/make_icon.py --dev` で作る） | jimmy |
| `~/.config/zcode-dev/args` | 引数（1 行目。無ければ `--keytest`） | jimmy |
| `~/.cache/zcode-dev/last.log` | 前回の標準エラー（`--stats` の結果など） | zcode |

- ランチャーの安全チェック（`cp0_desktop_exec_policy`）は「シェルの特殊記号が無い」
  「絶対パスなら実行可能なファイル」だけなので、ホームの下を指してよい
- アイコンは `/usr/share/APPLaunch/` の下にしか置けない。ランチャーは画像を LVGL の
  ドライブ `A:`（= `/usr/share/APPLaunch/`）経由で読むので、ホームを指す絶対パスを
  `Icon=` に書くと、登録は通るが画像が読めず既定の箱のアイコンになる（`..` で外へ出る
  指定は弾かれる）。そこで画像フォルダにホームのアイコンへのシンボリックリンクを置く
- ランチャーはユーザーのサービス（jimmy）として動き、起動したアプリも jimmy で動く。
  root 権限を渡す仕組みではない（sudo の設定は変えない）
- Esc を約 3 秒押し続けると、ランチャーがアプリを強制終了する（本体共通）

## 最初の 1 回（ユーザーが実機で行う）

```sh
sudo install -m 644 /tmp/zcode-dev.desktop /usr/share/APPLaunch/applications/zcode-dev.desktop
sudo ln -sf /home/jimmy/.local/lib/zcode-dev/icon.png /usr/share/APPLaunch/share/images/zcode-dev.png
systemctl --user restart APPLaunch.service
```

## 差し替え（SSH から、sudo なし）

```sh
python3 tools/make_icon.py --dev out/zcode-dev-icon.png   # 開発コンテナで
scp out/zcode-dev-icon.png <実機>:.local/lib/zcode-dev/icon.png
scp out/zcode <実機>:.local/lib/zcode-dev/zcode
ssh <実機> 'echo "--keytest --stats" > ~/.config/zcode-dev/args'
```

ユーザーがランチャーで「zcode (dev)」を選ぶと、その引数で起動する。結果は
`ssh <実機> cat ~/.cache/zcode-dev/last.log`。

## 片付け

```sh
sudo rm /usr/share/APPLaunch/applications/zcode-dev.desktop /usr/share/APPLaunch/share/images/zcode-dev.png
systemctl --user restart APPLaunch.service
rm -rf ~/.local/bin/zcode-dev ~/.local/lib/zcode-dev ~/.config/zcode-dev ~/.cache/zcode-dev
```
