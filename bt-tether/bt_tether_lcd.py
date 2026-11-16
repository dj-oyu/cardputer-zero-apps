#!/usr/bin/env python3
"""BT Tether — iPhone のインターネット共有へ Bluetooth PAN で
つなぐ APPLaunch アプリ (本体液晶版)。

使い方:
  - ペアリング済みデバイスが無い間は本体を発見可能にして待つ。
    iPhone の 設定 > Bluetooth でこの本体 (例: CardputerZero-2)
    を選び、数字照合が出たら本体の Enter で承認する。
    あわせて iPhone の「インターネット共有」を ON にしておく。
  - ペアリング後は Enter で接続 / 切断。
  - アプリを終了しても PAN 接続は bluetoothd 側に残る
    (このアプリは操作パネル)。切断はアプリから行う。

バックエンドは bt_tether.py (BlueZ D-Bus 直叩き)。
描画の土台は common/zero_ui.py。
"""

from __future__ import annotations

import os
import sys
import threading
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
_COMMON = os.path.normpath(os.path.join(_HERE, "..", "common"))
if os.path.isdir(_COMMON):
    sys.path.insert(0, _COMMON)

import numpy as np  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402

try:
    import bt_tether as bt  # noqa: E402
except ImportError:  # 描画テスト用 (VM には dbus が無い)
    bt = None  # type: ignore
import zero_ui as ui  # noqa: E402
from zero_ui import ENTER_KEYS, EXIT_KEYS, LCD_H, LCD_W  # noqa: E402

BG = (10, 13, 11)
FG = (235, 240, 236)
SUB = (150, 160, 152)
ACCENT = ui.ACCENT


# ------------------------------------------------------------------
# 描画 (view dict から 1 フレーム作る純粋関数)
# ------------------------------------------------------------------
def render_view(view: dict) -> Image.Image:
    img = Image.new("RGB", (LCD_W, LCD_H), BG)
    d = ImageDraw.Draw(img)
    d.text((8, 6), "BT Tether", font=ui.font(15), fill=FG)
    dot = ACCENT if view.get("powered", True) else (120, 60, 60)
    d.ellipse([LCD_W - 18, 9, LCD_W - 8, 19], fill=dot)
    d.line([(0, 27), (LCD_W, 27)], fill=(50, 58, 52))

    mode = view["mode"]
    if mode == "wait":
        d.text((8, 38), "ペアリング待ち", font=ui.font(19), fill=ACCENT)
        lines = [
            "iPhone の 設定 > Bluetooth で",
            f"「{view['adapter_name']}」を選んでください",
            "数字が出たら Enter で承認します",
            "インターネット共有も ON に",
        ]
        y = 70
        for ln in lines:
            d.text((8, y), ln, font=ui.font(12), fill=FG)
            y += 19
    elif mode == "pair":
        d.text((8, 36), "この数字が iPhone と一致したら",
               font=ui.font(13), fill=FG)
        pk = view["passkey"]
        f = ui.font(40)
        w = d.textlength(pk, font=f)
        d.text(((LCD_W - w) / 2, 58), pk, font=f, fill=ACCENT)
        d.text((8, 118), "[Enter] 一致する      [Esc] 拒否",
               font=ui.font(12), fill=SUB)
    elif mode == "ready":
        d.text((8, 38), view["device_name"], font=ui.font(19), fill=FG)
        d.text((8, 66), "未接続", font=ui.font(14), fill=SUB)
        d.text((8, 108), "[Enter] 接続", font=ui.font(14), fill=ACCENT)
    elif mode == "connecting":
        d.text((8, 38), view["device_name"], font=ui.font(19), fill=FG)
        d.text((8, 66), "接続中…", font=ui.font(14), fill=ACCENT)
        d.text((8, 108), "iPhone の共有が ON か確認してください",
               font=ui.font(12), fill=SUB)
    elif mode == "connected":
        d.text((8, 36), view["device_name"], font=ui.font(17), fill=FG)
        d.text((8, 60), "接続中", font=ui.font(13), fill=ACCENT)
        ip = view.get("ip")
        if ip:
            d.text((8, 84), f"{view['interface']}  {ip}",
                   font=ui.font(15), fill=FG)
        else:
            d.text((8, 84), f"{view['interface']}  IP 取得中…",
                   font=ui.font(15), fill=SUB)
        if view.get("elapsed"):
            d.text((8, 106), f"経過 {view['elapsed']}",
                   font=ui.font(12), fill=SUB)
        d.text((8, 128), "[Enter] 切断", font=ui.font(12), fill=SUB)
    elif mode == "error":
        d.text((8, 38), "エラー", font=ui.font(19), fill=(255, 120, 110))
        y = 68
        for ln in ui.wrap_text(d, view["error"], ui.font(12), LCD_W - 16):
            d.text((8, y), ln, font=ui.font(12), fill=FG)
            y += 17
        d.text((8, 128), "いずれかのキーで戻る", font=ui.font(12),
               fill=SUB)
    return img


def _status_of(view: dict) -> str:
    return {
        "wait": "BT Tether: ペアリング待ち",
        "pair": "BT Tether: ペアリング確認",
        "ready": "BT Tether: 未接続",
        "connecting": "BT Tether: 接続中",
        "connected": "BT Tether: 接続中",
        "error": "BT Tether: エラー",
    }[view["mode"]]


# ------------------------------------------------------------------
# 本体
# ------------------------------------------------------------------
def run_lcd() -> int:
    import dbus

    if bt is None:
        print("bt_tether backend unavailable (dbus missing)",
              file=sys.stderr)
        return 1
    state: dict = {
        "stop": False,
        "snap": None,
        "pair": None,          # {"device","passkey","reply","error"}
        "pair_show": None,     # 表示専用 passkey
        "connecting": False,
        "connecting_since": 0.0,
        "error": None,
        "connected_since": None,
        "lock": threading.Lock(),
    }

    def rejected():
        return dbus.exceptions.DBusException(
            "rejected by user", name="org.bluez.Error.Rejected")

    def on_confirm(device, passkey, reply, error):
        with state["lock"]:
            state["pair"] = {"device": device, "passkey": passkey,
                              "reply": reply, "error": error}

    def on_display(device, passkey):
        with state["lock"]:
            state["pair_show"] = passkey

    def on_cancel():
        with state["lock"]:
            pair = state["pair"]
            state["pair"] = None
            state["pair_show"] = None
        if pair is not None:
            try:
                pair["error"](rejected())
            except Exception:
                pass

    tether = bt.BlueZTether(on_confirm, on_display, on_cancel)
    tether.start()

    snap0 = tether.snapshot()
    if not snap0["powered"]:
        try:
            tether.set_powered(True)
        except Exception as exc:  # 電源投入に失敗しても画面は出す
            print(f"set_powered failed: {exc}", file=sys.stderr)

    def resolve_pair(accept: bool) -> None:
        with state["lock"]:
            pair = state["pair"]
            state["pair"] = None
            state["pair_show"] = None
        if pair is None:
            return
        try:
            if accept:
                pair["reply"]()
            else:
                pair["error"](rejected())
        except Exception:
            pass

    def poll_loop() -> None:
        discoverable_now = None
        while not state["stop"]:
            try:
                snap = tether.snapshot()
                with state["lock"]:
                    state["snap"] = snap
                # ペアリング済みが無い間だけ発見可能にする
                want_disc = tether.target(snap) is None
                if want_disc != discoverable_now:
                    tether.set_discoverable(want_disc)
                    discoverable_now = want_disc
                # 接続の経過管理
                tgt = tether.target(snap)
                with state["lock"]:
                    if tgt and tgt["net_connected"]:
                        if state["connected_since"] is None:
                            state["connected_since"] = time.time()
                        state["connecting"] = False
                    else:
                        state["connected_since"] = None
                    if (state["connecting"]
                            and time.time() - state["connecting_since"]
                            > 30):
                        state["connecting"] = False
                        state["error"] = "接続がタイムアウトしました"
            except Exception as exc:
                print(f"poll: {exc}", file=sys.stderr)
            time.sleep(1.0)

    def do_connect() -> None:
        snap = state["snap"]
        tgt = tether.target(snap) if snap else None
        if tgt is None:
            return
        with state["lock"]:
            state["connecting"] = True
            state["connecting_since"] = time.time()
            state["error"] = None

        def on_err(exc):
            with state["lock"]:
                state["connecting"] = False
                name = (exc.get_dbus_name()
                        if hasattr(exc, "get_dbus_name") else str(exc))
                state["error"] = f"接続できませんでした ({name})"

        tether.connect_nap(tgt["path"], lambda iface: None, on_err)

    def do_disconnect() -> None:
        snap = state["snap"]
        tgt = tether.target(snap) if snap else None
        if tgt is not None:
            try:
                tether.disconnect_nap(tgt["path"])
            except Exception as exc:
                with state["lock"]:
                    state["error"] = f"切断できませんでした ({exc})"

    def build_view() -> dict:
        with state["lock"]:
            snap = state["snap"]
            pair = state["pair"]
            pair_show = state["pair_show"]
            error = state["error"]
            connecting = state["connecting"]
            since = state["connected_since"]
        view: dict = {"powered": bool(snap and snap["powered"]),
                      "adapter_name": (snap or {}).get(
                          "adapter_name", "CardputerZero")}
        if pair is not None or pair_show is not None:
            pk = pair["passkey"] if pair else pair_show
            view.update(mode="pair", passkey=f"{int(pk):06d}")
            return view
        if error:
            view.update(mode="error", error=error)
            return view
        tgt = tether.target(snap) if snap else None
        if tgt is None:
            view.update(mode="wait")
            return view
        view["device_name"] = tgt["name"]
        if tgt["net_connected"]:
            elapsed = ""
            if since:
                sec = int(time.time() - since)
                elapsed = f"{sec // 60:02d}:{sec % 60:02d}"
            view.update(mode="connected",
                        interface=tgt["interface"] or "bnep0",
                        ip=bt.ipv4_of(tgt["interface"])
                        if tgt["interface"] else None,
                        elapsed=elapsed)
        elif connecting:
            view.update(mode="connecting")
        else:
            view.update(mode="ready")
        return view

    def key_loop() -> None:
        try:
            reader = ui.KeyReader()
        except OSError:
            return
        while not state["stop"]:
            for code in reader.poll():
                with state["lock"]:
                    has_pair = state["pair"] is not None
                    has_error = state["error"] is not None
                if has_pair:
                    if code in ENTER_KEYS:
                        resolve_pair(True)
                    elif code == 1:  # Esc
                        resolve_pair(False)
                    continue
                if has_error:
                    with state["lock"]:
                        state["error"] = None
                    continue
                view = build_view()
                if code in ENTER_KEYS:
                    if view["mode"] == "ready":
                        do_connect()
                    elif view["mode"] == "connected":
                        do_disconnect()
                elif code in EXIT_KEYS:
                    state["stop"] = True
                    break
            time.sleep(0.05)
        reader.close()

    threading.Thread(target=poll_loop, daemon=True).start()
    threading.Thread(target=key_loop, daemon=True).start()

    fb = ui.FrameBuffer()
    try:
        while not state["stop"]:
            view = build_view()
            img = render_view(view)
            ui.finish_frame(img, 0, _status_of(view), None, None, None, time.time())
            fb.write(ui.to_rgb565(np.asarray(img)))
            time.sleep(0.25)
    finally:
        state["stop"] = True
        try:
            tether.set_discoverable(False)
        except Exception:
            pass
        fb.close()
    return 0


# ------------------------------------------------------------------
# 描画テスト (実機なしで全状態を PNG に出す)
# ------------------------------------------------------------------
def render_test(out_prefix: str) -> int:
    views = {
        "wait": {"mode": "wait", "powered": True,
                 "adapter_name": "CardputerZero-2"},
        "pair": {"mode": "pair", "powered": True, "passkey": "483920"},
        "ready": {"mode": "ready", "powered": True,
                  "device_name": "iPhone"},
        "connecting": {"mode": "connecting", "powered": True,
                       "device_name": "iPhone"},
        "connected": {"mode": "connected", "powered": True,
                      "device_name": "iPhone", "interface": "bnep0",
                      "ip": "172.20.10.2", "elapsed": "03:25"},
        "error": {"mode": "error", "powered": True,
                  "error": "接続できませんでした "
                           "(org.bluez.Error.Failed)"},
    }
    for name, view in views.items():
        img = render_view(view)
        ui.finish_frame(img, 0, _status_of(view), None, None, None, time.time())
        path = f"{out_prefix}_{name}.png"
        img.save(path)
        print(f"render-test -> {path}")
    return 0


def main() -> int:
    if "--render-test" in sys.argv:
        i = sys.argv.index("--render-test")
        return render_test(sys.argv[i + 1])
    try:
        return run_lcd()
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
