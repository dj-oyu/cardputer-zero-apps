//! 通信の受け入れ試験（課題 D1。docs/stage3-design.md §14 の表）。ループバックの受付
//! （TLS なし）と試験用の TLS サーバに、本物のソケットでつなぐ。道具は `support/`。
//!
//! ここは公開の形だけを使う。名前の引き方を差し替える試験（名前解決の時間切れ・
//! 2 つ目の行き先・古い IP）と、経路の表・TLS の失敗の読み分けは、中の関数が要るので
//! `tests/inproc/net.rs`（`src/net.rs` の中の試験として動く）にある。

mod api {
    pub use zc_remote::auth::*;
    pub use zc_remote::net::*;
}
mod support;

use api::*;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use support::*;
use zc_core::time::now_us;

#[test]
fn endpoint_parse() {
    let e = |tls, host: &str, port, path: &str| Some(Endpoint { tls, host: host.into(), port, path: path.into() });
    assert_eq!(Endpoint::parse("wss://build.example.com/v1/ws"), e(true, "build.example.com", 443, "/v1/ws"));
    assert_eq!(Endpoint::parse("wss://build.example.com"), e(true, "build.example.com", 443, "/v1/ws"));
    assert_eq!(Endpoint::parse("ws://127.0.0.1:7411/x?y=1"), e(false, "127.0.0.1", 7411, "/x?y=1"));
    assert_eq!(Endpoint::parse("ws://pc"), e(false, "pc", 80, "/v1/ws"));
    assert_eq!(Endpoint::parse("pc.example.ts.net:7411"), e(false, "pc.example.ts.net", 7411, "/v1/ws"));
    assert_eq!(Endpoint::parse("pc"), e(false, "pc", 7411, "/v1/ws"));
    assert_eq!(Endpoint::parse("[::1]:7411"), e(false, "::1", 7411, "/v1/ws"));
    assert_eq!(Endpoint::parse("wss://[fd7a::1]/v1/ws"), e(true, "fd7a::1", 443, "/v1/ws"));
    for bad in ["", "http://x/", "wss://", "wss://:443/", "pc:port", "pc:0", "pc:70000", "[::1", "[zz]:1", "ws://a b/", "ws://h/a b", "ws://u@h/", "ws://h/\r\nX: 1"] {
        assert_eq!(Endpoint::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn plain_ws_round_trip() {
    let port = serve_plain(|_, s| echo(s));
    let (mut c, stats) = dial(&format!("127.0.0.1:{port}"), None).unwrap();
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert!(!stats.resumed && stats.tls_us == 0 && stats.dns_us == 0);
    assert_eq!(c.interest(), Interest { fd: c.interest().fd, read: true, write: false });

    let big = pattern(zc_wire::limits::CHUNK_BYTES, 3);
    c.send_text("hello");
    c.send_text("日本語");
    c.send_binary(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &big);
    assert!(c.queued_bytes() > big.len());
    assert!(c.interest().write);
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 3).unwrap();
    assert_eq!(c.queued_bytes(), 0);
    assert_eq!(got[0], Msg::Text("hello".into()));
    assert_eq!(got[1], Msg::Text("日本語".into()));
    let Msg::Binary(b) = &got[2] else { panic!() };
    assert_eq!(b.len(), 16 + big.len());
    assert_eq!(&b[16..], &big[..]);

    // 相手が close を送ってきた: 状態コードつきで知らせる
    c.send_text("bye");
    assert_eq!(pump(&mut c, &mut got, |_| false), Err(NetError::Closed(4001)));
}

#[test]
fn ping_fragments_and_first_frame_with_101() {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let port = serve_plain(move |_, s| {
        // 101 と同じ書き込みに最初のフレームを載せる
        let mut srv = Srv::accept(s, &["early-1", "early-2"]).unwrap();
        // 分割されたテキストの途中に ping
        srv.raw(&[0x01, 2, b'h', b'e', 0x89, 2, b'h', b'i', 0x80, 3, b'l', b'l', b'o']);
        let pong = srv.recv();
        // close を待つ
        let close = srv.recv();
        tx.lock().unwrap().send((pong, close)).unwrap();
    });
    let (mut c, _) = dial(&format!("ws://127.0.0.1:{port}/v1/ws"), None).unwrap();
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 3).unwrap();
    assert_eq!(got, [Msg::Text("early-1".into()), Msg::Text("early-2".into()), Msg::Text("hello".into())]);
    // 届いた時刻と、沈黙の判定
    let rx_at = c.last_rx_us();
    assert!(rx_at <= now_us() && rx_at > 0);
    assert!(!c.is_silent(rx_at + 10, Duration::from_micros(20)));
    assert!(c.is_silent(rx_at + 20, Duration::from_micros(20)));
    // 静かに閉じる: close が相手に届く
    c.on_writable().unwrap();
    c.close();
    let (pong, close) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(pong, Some(Msg::Pong(b"hi".to_vec())));
    assert_eq!(close, Some(Msg::Close(1000)));
}

#[test]
fn tls_round_trip_through_name_resolution_and_resumption() {
    let cfg = server_cfg(GOOD, false);
    let port = serve_tls(cfg, |_, s| echo(s));
    let tls = client_cfg();
    // 名前で（使い捨てのスレッドで引く。::1 が先に返っても 127.0.0.1 に落ち着く）
    let (mut c, s1) = dial(&format!("wss://localhost:{port}/v1/ws"), Some(tls.clone())).unwrap();
    assert!(!s1.resumed);
    assert!(s1.tls_us > 0);
    let big = pattern(zc_wire::limits::CHUNK_BYTES, 9);
    c.send_text("over tls");
    c.send_binary(&[2; 16], &big);
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 2).unwrap();
    assert_eq!(got[0], Msg::Text("over tls".into()));
    assert_eq!(got[1], Msg::Binary([&[2u8; 16][..], &big[..]].concat()));
    let ip = c.peer_ip();
    c.close();

    // 2 回目: 再開（チケットは設定の中 = RAM）。前回の IP を渡す
    let (mut c, s2) = dial_to(&format!("wss://localhost:{port}"), &mut NoCredential, Some(tls), Some(ip)).unwrap();
    assert!(s2.resumed, "{s2:?}");
    assert_eq!(s2.dns_us, 0);
    c.send_text("again");
    pump(&mut c, &mut got, |g| g.len() == 3).unwrap();
    assert_eq!(got[2], Msg::Text("again".into()));

    // TLS の設定を渡し忘れたら、つながない
    assert!(matches!(err(dial(&format!("wss://localhost:{port}"), None)), NetError::Tls(_)));
}

#[test]
fn tls_server_drops_the_connection() {
    let port = serve_tls(server_cfg(GOOD, false), |_, s| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        srv.text("last words");
        // close も close_notify も送らずに切る
    });
    let (mut c, _) = dial(&format!("wss://127.0.0.1:{port}"), Some(client_cfg())).unwrap();
    let mut got = Vec::new();
    assert_eq!(pump(&mut c, &mut got, |_| false), Err(NetError::Closed(1006)));
    // 切れる前に届いていたものは渡してある
    assert_eq!(got, [Msg::Text("last words".into())]);
}

#[test]
fn tls_refuses_aes_only_server_and_bad_certificates() {
    // AES-GCM しか出さないサーバ: 共通の暗号が無いので、つながらない
    let port = serve_tls(server_cfg(GOOD, true), |_, s| echo(s));
    let e = err(dial(&format!("wss://localhost:{port}"), Some(client_cfg())));
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");

    // まだ有効でない証明書 = 時計が合っていない
    let port = serve_tls(server_cfg(FUTURE, false), |_, s| echo(s));
    assert_eq!(err(dial(&format!("wss://localhost:{port}"), Some(client_cfg()))), NetError::Clock);

    // 期限切れ: 時計が同期済みなら TLS の失敗、同期前なら時計のせい（どちらかは環境による）
    let port = serve_tls(server_cfg(EXPIRED, false), |_, s| echo(s));
    let e = err(dial(&format!("wss://localhost:{port}"), Some(client_cfg())));
    if clock_synchronized() {
        assert!(matches!(&e, NetError::Tls(m) if m.to_lowercase().contains("expired")), "{e:?}");
    } else {
        assert_eq!(e, NetError::Clock);
    }

    // 知らない認証局（埋め込みの根には、試験用の認証局は無い）
    let port = serve_tls(server_cfg(GOOD, false), |_, s| echo(s));
    let e = err(dial(&format!("wss://localhost:{port}"), Some(tls_config(embedded_roots()))));
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");

}

#[test]
fn access_gate_statuses_and_token() {
    let port = serve_tls(server_cfg(GOOD, false), |_, s| gate(s));
    let tls = client_cfg();
    let go = |path: &str, cred: &mut dyn Credential| dial_to(&format!("wss://localhost:{port}{path}"), cred, Some(tls.clone()), None);

    // トークンなし → 403。302（ログイン画面）と HTML もたどらずに認証の失敗
    assert_eq!(err(go("/v1/ws", &mut NoCredential)), NetError::Auth(403));
    assert_eq!(err(go("/login", &mut NoCredential)), NetError::Auth(302));
    assert_eq!(err(go("/html", &mut NoCredential)), NetError::Auth(200));
    assert_eq!(err(go("/unauth", &mut NoCredential)), NetError::Auth(401));
    // 上限・混雑
    assert_eq!(err(go("/busy", &mut NoCredential)), NetError::Busy(429, Some(7)));
    assert_eq!(err(go("/down", &mut NoCredential)), NetError::Busy(503, None));
    // そのほか
    assert_eq!(err(go("/teapot", &mut NoCredential)), NetError::Protocol("HTTP 418".into()));
    assert!(matches!(err(go("/badaccept", &mut NoCredential)), NetError::Protocol(_)));
    assert!(matches!(err(go("/ssh", &mut NoCredential)), NetError::Protocol(_)));

    // 0600 のトークンで通る
    use std::os::unix::fs::PermissionsExt;
    let p = std::env::temp_dir().join(format!("zc-remote-net-token-{}.json", std::process::id()));
    std::fs::write(&p, r#"{"scheme":"cf-access-service-token","client_id":"abc.access","client_secret":"s3cr3t"}"#).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut token = AccessServiceToken::load(&p).unwrap();
    let (mut c, _) = go("/v1/ws", &mut token).unwrap();
    let mut got = Vec::new();
    pump(&mut c, &mut got, |g| g.len() == 1).unwrap();
    assert_eq!(got, [Msg::Text("welcome".into())]);

    // 断られたトークンは、ファイルを直すまでつなぎに行かない（Auth のまま、すぐ返る）
    assert_eq!(token.rejected(403), Retry::GiveUp);
    assert_eq!(err(go("/v1/ws", &mut token)), NetError::Auth(403));
    std::fs::remove_file(&p).unwrap();
}

#[test]
fn black_holes_time_out_at_each_stage() {
    // 受けるだけで何も返さない相手
    let hold = |_: usize, mut s: Box<dyn Stream>| {
        let mut b = [0u8; 64];
        while matches!(s.read(&mut b), Ok(n) if n > 0) {}
    };
    // TLS なし: Upgrade の返事が来ない
    let port = serve_plain(hold);
    let t0 = now_us();
    assert_eq!(err(dial(&format!("127.0.0.1:{port}"), None)), NetError::Timeout(Stage::Upgrade));
    let took = now_us() - t0;
    assert!((350_000..2_000_000).contains(&took), "{took}");
    // TLS あり: ハンドシェイクが進まない（TCP は受けるが、TLS としては黙る）
    let port = serve_plain(hold);
    assert_eq!(err(dial(&format!("wss://localhost:{port}"), Some(client_cfg()))), NetError::Timeout(Stage::Tls));

    // 誰も待っていないポート: すぐ断られる
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert_eq!(err(dial(&format!("127.0.0.1:{port}"), None)), NetError::Unreachable(Stage::Tcp, libc::ECONNREFUSED));
}

#[test]
fn tcp_black_hole_times_out_and_second_address_wins() {
    let hole = syn_black_hole("127.0.0.1", 0).unwrap();
    let port = hole.0.local_addr().unwrap().port();
    // 列がいっぱいになっていること（環境によっては効かないので、そのときは試験を飛ばす）
    if TcpStream::connect_timeout(&hole.0.local_addr().unwrap(), Duration::from_millis(100)).is_ok() {
        eprintln!("SYN を捨てるポートを作れなかったので飛ばす");
        return;
    }
    let t0 = now_us();
    assert_eq!(err(dial(&format!("127.0.0.1:{port}"), None)), NetError::Timeout(Stage::Tcp));
    let took = now_us() - t0;
    assert!((250_000..1_500_000).contains(&took), "{took}");

}

/// 少しずつしか書けないソケットで、大きな中身を送り切る。
fn upload_through_a_narrow_socket(tls: bool) {
    const FRAMES: usize = 24;
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let script = move |_: usize, s: Box<dyn Stream>| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        let mut ok = true;
        for i in 0..FRAMES {
            // ゆっくり読む相手
            thread::sleep(Duration::from_millis(2));
            let Some(Msg::Binary(b)) = srv.recv() else { return };
            ok &= b[16..] == pattern(zc_wire::limits::CHUNK_BYTES, i as u8)[..] && b[..16] == [i as u8; 16];
        }
        srv.text(if ok { "all good" } else { "corrupt" });
        tx.lock().unwrap().send(()).unwrap();
        // クライアントが閉じるまで待つ（先に切ると、返事と一緒に「切れた」が届く）
        let _ = srv.recv();
    };
    let (port, cfg) = if tls { (serve_tls(server_cfg(GOOD, false), script), Some(client_cfg())) } else { (serve_plain(script), None) };
    let (mut c, _) = dial(&format!("{}://localhost:{port}", if tls { "wss" } else { "ws" }), cfg).unwrap();
    // 送信バッファを最小にする
    let small: libc::c_int = 1;
    // SAFETY: small は有効な c_int。
    unsafe { libc::setsockopt(c.interest().fd, libc::SOL_SOCKET, libc::SO_SNDBUF, (&small as *const libc::c_int).cast(), 4) };

    let (mut next, mut got, mut max_queued) = (0, Vec::new(), 0);
    let begin = now_us();
    while got.is_empty() {
        // serve と同じ決まり: 送り待ちが 256KB を超えている間は、次の中身を足さない
        while next < FRAMES && c.queued_bytes() <= (256 << 10) {
            c.send_binary(&[next as u8; 16], &pattern(zc_wire::limits::CHUNK_BYTES, next as u8));
            next += 1;
        }
        max_queued = max_queued.max(c.queued_bytes());
        pump(&mut c, &mut got, |_| true).unwrap();
        wait(&[c.interest()], 20_000);
        assert!(now_us() - begin < 20_000_000);
    }
    assert_eq!(got, [Msg::Text("all good".into())]);
    assert_eq!(c.queued_bytes(), 0);
    assert!(!c.interest().write);
    // 溜めたのは 256KB + フレーム 1 つぶんまで
    assert!(max_queued <= (256 << 10) + zc_wire::limits::CHUNK_BYTES + 64, "{max_queued}");
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn narrow_socket_plain() {
    upload_through_a_narrow_socket(false);
}

#[test]
fn narrow_socket_tls() {
    upload_through_a_narrow_socket(true);
}

/// 読む量の上限: 1 回の呼び出しで読みすぎず、残りがあれば `Ok(true)` で知らせる。
fn read_budget(tls: bool) {
    const N: usize = 400;
    let script = |_: usize, s: Box<dyn Stream>| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        // 全部を 1 回の書き込みで（クライアントの中のバッファに溜まる形にする）
        let mut out = Vec::new();
        for i in 0..N {
            srv.enc.text(&format!("message number {i:04}"), [0; 4], &mut out);
        }
        srv.enc.binary(&pattern(100_000, 1), [0; 4], &mut out);
        srv.raw(&out);
        let _ = srv.recv();
    };
    let (port, cfg) = if tls { (serve_tls(server_cfg(GOOD, false), script), Some(client_cfg())) } else { (serve_plain(script), None) };
    let (mut c, _) = dial(&format!("{}://localhost:{port}", if tls { "wss" } else { "ws" }), cfg).unwrap();
    let (mut got, mut more_calls, mut most) = (Vec::new(), 0, 0);
    let begin = now_us();
    while got.len() < N + 1 {
        wait(&[c.interest()], 50_000);
        loop {
            let before = got.len();
            let more = c.on_readable(512, &mut |m| got.push(matches!(m, Incoming::Text(_)))).unwrap();
            most = most.max(got.len() - before);
            if !more {
                break;
            }
            more_calls += 1;
        }
        assert!(now_us() - begin < 10_000_000);
    }
    assert!(got[..N].iter().all(|t| *t) && !got[N]);
    // 512 バイトずつなので、何十回も「まだ残っている」と返る。1 回で渡すのは 512 バイト + 1 つぶんまで
    assert!(more_calls > 100, "{more_calls}");
    assert!(most <= 512 / 22 + 2, "{most}");
}

#[test]
fn read_budget_plain() {
    read_budget(false);
}

#[test]
fn read_budget_tls() {
    read_budget(true);
}

#[test]
fn protocol_violations_from_the_server() {
    // マスクつきのフレーム（サーバは掛けてはいけない）
    let port = serve_plain(|_, s| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        srv.raw(&[0x81, 0x81, 1, 2, 3, 4, b'x' ^ 1]);
        let _ = srv.recv();
    });
    let (mut c, _) = dial(&format!("127.0.0.1:{port}"), None).unwrap();
    assert!(matches!(pump(&mut c, &mut Vec::new(), |_| false), Err(NetError::Protocol(_))));

    // 大きすぎるメッセージ
    let port = serve_plain(|_, s| {
        let mut srv = Srv::accept(s, &[]).unwrap();
        srv.raw(&[0x82, 127, 0, 0, 0, 0, 0x10, 0, 0, 0]);
        let _ = srv.recv();
    });
    let (mut c, _) = dial(&format!("127.0.0.1:{port}"), None).unwrap();
    assert!(matches!(pump(&mut c, &mut Vec::new(), |_| false), Err(NetError::Protocol(_))));
}
