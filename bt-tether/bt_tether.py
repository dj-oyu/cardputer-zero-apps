#!/usr/bin/env python3
"""BT テザリングのバックエンド (BlueZ を D-Bus で直接叩く)。

NetworkManager は polkit で認証止まり (実機確認済み) のため
使わない。ペアリングのエージェント (数字照合の確認) と
PAN (NAP) への接続を org.bluez の API で行う。

- 状態は ObjectManager のポーリングで読む (読みは権限不要)
- ペアリング確認はコールバック経由でアプリに渡し、アプリが
  ユーザーの Enter/Esc で reply/error を返す (非同期)
- GLib メインループがエージェントの受け口になるので、必ず
  start() で別スレッドのループを回すこと
"""

from __future__ import annotations

import fcntl
import socket
import struct
import threading

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

BLUEZ = "org.bluez"
ADAPTER_PATH = "/org/bluez/hci0"
AGENT_PATH = "/org/bluez/agent_bttether"
PAN_UUID = "00001115-0000-1000-8000-00805f9b34fb"  # NAP


def ipv4_of(ifname: str) -> str | None:
    """インターフェースの IPv4 アドレス (無ければ None)。"""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        packed = fcntl.ioctl(
            s.fileno(), 0x8915,  # SIOCGIFADDR
            struct.pack("256s", ifname[:15].encode()))
        return socket.inet_ntoa(packed[20:24])
    except OSError:
        return None
    finally:
        s.close()


class PairingAgent(dbus.service.Object):
    """org.bluez.Agent1 の実装 (DisplayYesNo)。

    on_confirm(device_path, passkey:int, reply, error) を
    アプリに渡す。アプリはユーザーの判断で reply() か
    error(exception) のどちらかを呼ぶ。
    on_display(device_path, passkey:int) は表示専用。
    on_cancel() は相手側キャンセルの通知。
    """

    def __init__(self, bus, path, on_confirm, on_display, on_cancel):
        super().__init__(bus, path)
        self._on_confirm = on_confirm
        self._on_display = on_display
        self._on_cancel = on_cancel

    @dbus.service.method("org.bluez.Agent1", in_signature="",
                         out_signature="")
    def Release(self):
        pass

    @dbus.service.method("org.bluez.Agent1", in_signature="o",
                         out_signature="s")
    def RequestPinCode(self, device):
        # レガシー PIN を求められた場合の既定値 (iPhone は使わない)
        return "0000"

    @dbus.service.method("org.bluez.Agent1", in_signature="o",
                         out_signature="u")
    def RequestPasskey(self, device):
        return dbus.UInt32(0)

    @dbus.service.method("org.bluez.Agent1", in_signature="ouq",
                         out_signature="")
    def DisplayPasskey(self, device, passkey, entered):
        self._on_display(str(device), int(passkey))

    @dbus.service.method("org.bluez.Agent1", in_signature="ou",
                         out_signature="",
                         async_callbacks=("reply", "error"))
    def RequestConfirmation(self, device, passkey, reply, error):
        self._on_confirm(str(device), int(passkey), reply, error)

    @dbus.service.method("org.bluez.Agent1", in_signature="o",
                         out_signature="",
                         async_callbacks=("reply", "error"))
    def RequestAuthorization(self, device, reply, error):
        reply()

    @dbus.service.method("org.bluez.Agent1", in_signature="os",
                         out_signature="",
                         async_callbacks=("reply", "error"))
    def AuthorizeService(self, device, uuid, reply, error):
        # ペアリング済み相手のサービス接続は許可する
        reply()

    @dbus.service.method("org.bluez.Agent1", in_signature="",
                         out_signature="")
    def Cancel(self):
        self._on_cancel()


class BlueZTether:
    def __init__(self, on_confirm, on_display, on_cancel):
        dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
        self.bus = dbus.SystemBus()
        self._loop = GLib.MainLoop()
        self._thread: threading.Thread | None = None
        self.agent = PairingAgent(self.bus, AGENT_PATH,
                                  on_confirm, on_display, on_cancel)
        mgr = dbus.Interface(
            self.bus.get_object(BLUEZ, "/org/bluez"),
            "org.bluez.AgentManager1")
        mgr.RegisterAgent(AGENT_PATH, "DisplayYesNo")
        try:
            mgr.RequestDefaultAgent(AGENT_PATH)
        except dbus.DBusException:
            pass  # 既定でなくても、こちら宛の要求には応答できる

    def start(self) -> None:
        self._thread = threading.Thread(target=self._loop.run,
                                        daemon=True)
        self._thread.start()

    # -- 小物 -------------------------------------------------
    def _props(self, path: str, iface: str) -> dbus.Interface:
        return dbus.Interface(self.bus.get_object(BLUEZ, path),
                              "org.freedesktop.DBus.Properties")

    def _adapter(self) -> dbus.Interface:
        return dbus.Interface(self.bus.get_object(BLUEZ, ADAPTER_PATH),
                              "org.bluez.Adapter1")

    def set_powered(self, on: bool) -> None:
        self._props(ADAPTER_PATH, "org.bluez.Adapter1").Set(
            "org.bluez.Adapter1", "Powered", dbus.Boolean(on))

    def set_discoverable(self, on: bool) -> None:
        p = self._props(ADAPTER_PATH, "org.bluez.Adapter1")
        p.Set("org.bluez.Adapter1", "Pairable", dbus.Boolean(True))
        if on:
            p.Set("org.bluez.Adapter1", "DiscoverableTimeout",
                  dbus.UInt32(0))
        p.Set("org.bluez.Adapter1", "Discoverable", dbus.Boolean(on))

    # -- 状態 -------------------------------------------------
    def snapshot(self) -> dict:
        """アダプタと既知デバイスの状態をまとめて返す。"""
        om = dbus.Interface(self.bus.get_object(BLUEZ, "/"),
                            "org.freedesktop.DBus.ObjectManager")
        objs = om.GetManagedObjects()
        out: dict = {"powered": False, "discoverable": False,
                     "adapter_name": "CardputerZero", "devices": []}
        for path, ifs in objs.items():
            if path == ADAPTER_PATH and "org.bluez.Adapter1" in ifs:
                a = ifs["org.bluez.Adapter1"]
                out["powered"] = bool(a.get("Powered", False))
                out["discoverable"] = bool(a.get("Discoverable", False))
                out["adapter_name"] = str(
                    a.get("Alias") or a.get("Name") or "CardputerZero")
            if "org.bluez.Device1" in ifs:
                d = ifs["org.bluez.Device1"]
                dev = {
                    "path": str(path),
                    "address": str(d.get("Address", "")),
                    "name": str(d.get("Alias") or d.get("Name")
                                or d.get("Address", "")),
                    "paired": bool(d.get("Paired", False)),
                    "connected": bool(d.get("Connected", False)),
                    "net_connected": False,
                    "interface": None,
                }
                if "org.bluez.Network1" in ifs:
                    n = ifs["org.bluez.Network1"]
                    dev["net_connected"] = bool(n.get("Connected", False))
                    iface = n.get("Interface")
                    dev["interface"] = str(iface) if iface else None
                out["devices"].append(dev)
        out["devices"].sort(key=lambda v: (not v["paired"],
                                           not v["net_connected"],
                                           v["name"]))
        return out

    def target(self, snap: dict | None = None) -> dict | None:
        """操作対象のデバイス (ペアリング済みを優先)。"""
        snap = snap if snap is not None else self.snapshot()
        for dev in snap["devices"]:
            if dev["paired"]:
                return dev
        return None

    # -- 接続 -------------------------------------------------
    def connect_nap(self, device_path: str, on_ok, on_err) -> None:
        """PAN (NAP) へ接続する。結果はコールバックで返す。"""
        net = dbus.Interface(self.bus.get_object(BLUEZ, device_path),
                             "org.bluez.Network1")
        net.Connect("nap",
                    reply_handler=lambda iface: on_ok(str(iface)),
                    error_handler=lambda exc: on_err(exc))

    def disconnect_nap(self, device_path: str) -> None:
        net = dbus.Interface(self.bus.get_object(BLUEZ, device_path),
                             "org.bluez.Network1")
        net.Disconnect()
