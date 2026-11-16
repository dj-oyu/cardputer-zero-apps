//! 通信の試験のうち、`net.rs` の中の関数が要るもの（課題 D1）。`src/net.rs` の中の試験
//! として動く（`#[path]` で読み込む）。公開の形だけで書ける試験は `tests/net.rs`。
//!
//! - 名前の引き方の差し替え（`Dial::start_with`）: 時間切れ・引けない・2 つ目の行き先・
//!   族をまたぐ・古い IP・前回の IP なら引かない
//! - TLS の失敗の読み分け（`classify_tls`）、経路の表の読み方（`default_route_in`）、
//!   `Host` ヘッダ

use super::support::*;
use super::*;
use crate::auth::{NoCredential, Retry};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

fn dial_with(ep: &Endpoint, cred: &mut dyn Credential, tls: Option<Arc<rustls::ClientConfig>>, hint: Option<IpAddr>, resolve: ResolveFn) -> Result<(Conn, DialStats), NetError> {
    drive(Dial::start_with(ep, cred, tls, fast(), hint, now_us(), resolve)?)
}

#[test]
fn host_header() {
    assert_eq!(Endpoint::parse("wss://h").unwrap().host_header(), "h");
    assert_eq!(Endpoint::parse("wss://h:8443").unwrap().host_header(), "h:8443");
    assert_eq!(Endpoint::parse("h:7411").unwrap().host_header(), "h:7411");
    assert_eq!(Endpoint::parse("[::1]:7411").unwrap().host_header(), "[::1]:7411");
}

#[test]
fn hint_skips_name_resolution() {
    let port = serve_tls(server_cfg(GOOD, false), |_, s| echo(s));
    fn must_not_resolve(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        panic!("前回の IP でつながるのに名前を引いた");
    }
    let ep = Endpoint::parse(&format!("wss://localhost:{port}")).unwrap();
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    let (c, stats) = dial_with(&ep, &mut NoCredential, Some(client_cfg()), Some(ip), must_not_resolve).unwrap();
    assert_eq!((c.peer_ip(), stats.dns_us), (ip, 0));
}

#[test]
fn certificate_name_mismatch() {
    let port = serve_tls(server_cfg(GOOD, false), |_, s| echo(s));
    // 名前が合わない（証明書は localhost と 127.0.0.1 / ::1 だけ）
    fn to_loopback(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
    }
    let ep = Endpoint::parse(&format!("wss://other.example:{port}")).unwrap();
    let e = err(dial_with(&ep, &mut NoCredential, Some(client_cfg()), None, to_loopback));
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");
}

#[test]
fn tls_errors_are_classified() {
    use rustls::pki_types::UnixTime;
    use rustls::CertificateError as C;
    let t = UnixTime::since_unix_epoch(Duration::from_secs(1));
    let cert = |c| rustls::Error::InvalidCertificate(c);
    for synced in [true, false] {
        assert_eq!(classify_tls(&cert(C::NotValidYet), synced), NetError::Clock);
        assert_eq!(classify_tls(&cert(C::NotValidYetContext { time: t, not_before: t }), synced), NetError::Clock);
        assert!(matches!(classify_tls(&cert(C::UnknownIssuer), synced), NetError::Tls(_)));
        assert!(matches!(classify_tls(&rustls::Error::AlertReceived(rustls::AlertDescription::HandshakeFailure), synced), NetError::Tls(_)));
    }
    assert!(matches!(classify_tls(&cert(C::Expired), true), NetError::Tls(_)));
    assert!(matches!(classify_tls(&cert(C::ExpiredContext { time: t, not_after: t }), true), NetError::Tls(_)));
    assert_eq!(classify_tls(&cert(C::Expired), false), NetError::Clock);
    assert_eq!(classify_tls(&cert(C::ExpiredContext { time: t, not_after: t }), false), NetError::Clock);
}

#[test]
fn name_resolution_times_out_or_fails() {
    // 名前解決が戻らない: スレッドは捨てて、時間切れ
    fn hang(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        thread::sleep(Duration::from_secs(3));
        Ok(Vec::new())
    }
    let ep = Endpoint::parse("slow-dns.example:7411").unwrap();
    let t0 = now_us();
    assert_eq!(err(dial_with(&ep, &mut NoCredential, None, None, hang)), NetError::Timeout(Stage::Dns));
    let took = now_us() - t0;
    assert!((250_000..1_500_000).contains(&took), "{took}");

    // 名前が無い
    fn nx(_: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        Err(io::Error::other("no such host"))
    }
    assert_eq!(err(dial_with(&ep, &mut NoCredential, None, None, nx)), NetError::Unreachable(Stage::Dns, 0));
}

#[test]
fn second_address_wins_when_the_first_is_silent() {
    // 1 つ目の行き先が黙っていても、少しずらして始めた 2 つ目でつながる
    // （127.0.0.2 の同じポートを黙らせ、127.0.0.1 で待つ）
    let (good_port, _hole2) = (0..20)
        .find_map(|_| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let p = l.local_addr().unwrap().port();
            let hole = syn_black_hole("127.0.0.2", p)?;
            serve_on(l, None, |_, s| echo(s));
            Some((p, hole))
        })
        .unwrap();
    fn hole_then_good(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::new("127.0.0.2".parse().unwrap(), port), SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
    }
    let ep = Endpoint::parse(&format!("two-addrs.example:{good_port}")).unwrap();
    let t0 = now_us();
    let (c, _) = dial_with(&ep, &mut NoCredential, None, None, hole_then_good).unwrap();
    let took = now_us() - t0;
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    // ずらす時間（40ms）は待つが、1 つ目の時間切れ（300ms）は待たない
    assert!((30_000..280_000).contains(&took), "{took}");
}

#[test]
fn falls_back_across_families_and_from_a_stale_hint() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    // ::1 では誰も待っていない（IPv6 が無い環境でも、その場で失敗して次へ進む）
    fn v6_then_v4(_: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![SocketAddr::new("::1".parse().unwrap(), port), SocketAddr::new("::1".parse().unwrap(), port), SocketAddr::new("127.0.0.1".parse().unwrap(), port)])
    }
    let port = serve_plain(|_, s| echo(s));
    let ep = Endpoint::parse(&format!("dual.example:{port}")).unwrap();
    let (c, stats) = dial_with(&ep, &mut NoCredential, None, None, v6_then_v4).unwrap();
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert!(stats.dns_us > 0);
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);

    // 前回の IP がもう違う（そこでは誰も待っていない）: 名前を引き直してつながる
    let stale: IpAddr = "127.0.0.3".parse().unwrap();
    let (c, _) = dial_with(&ep, &mut NoCredential, None, Some(stale), v6_then_v4).unwrap();
    assert_eq!(c.peer_ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);

    // 資格を用意できないときは、つなぎに行かない
    struct Broken;
    impl Credential for Broken {
        fn headers(&mut self, _: &mut Vec<(String, String)>) -> Result<(), AuthError> {
            Err(AuthError::Missing)
        }
        fn rejected(&mut self, _: u16) -> Retry {
            Retry::GiveUp
        }
    }
    assert_eq!(err(dial_with(&ep, &mut Broken, None, None, v6_then_v4)), NetError::Auth(0));
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
}

#[test]
fn default_route_detection() {
    let dir = std::env::temp_dir().join(format!("zc-remote-net-route-{}", std::process::id()));
    let (proc_net, sys_net) = (dir.join("proc"), dir.join("sys"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&proc_net).unwrap();
    let oper = |iface: &str, state: &str| {
        std::fs::create_dir_all(sys_net.join(iface)).unwrap();
        std::fs::write(sys_net.join(iface).join("operstate"), format!("{state}\n")).unwrap();
    };
    let check = || default_route_in(&proc_net, &sys_net);
    const HEAD: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
    let v4 = |rows: &str| std::fs::write(proc_net.join("route"), format!("{HEAD}{rows}")).unwrap();
    let v6 = |rows: &str| std::fs::write(proc_net.join("ipv6_route"), rows).unwrap();
    const LAN: &str = "wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n";
    const V6_LO: &str = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";

    // 表が読めない（/proc が無い）: 止めない
    assert!(check());
    // 既定の経路が無い（LAN の経路だけ）
    v4(LAN);
    assert!(!check());
    // Wi-Fi の既定の経路
    v4(&format!("wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n{LAN}"));
    oper("wlan0", "up");
    assert!(check());
    // インターフェースが落ちている
    oper("wlan0", "down");
    assert!(!check());
    oper("wlan0", "dormant");
    assert!(!check());
    // Bluetooth のテザリング（bnep0）は状態が unknown でも通れる
    v4("wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\nbnep0\t00000000\t01140AAC\t0003\t0\t0\t700\t00000000\t0\t0\t0\n");
    oper("bnep0", "unknown");
    assert!(check());
    // 経路はあるが UP でない
    v4("wlan0\t00000000\t0101A8C0\t0002\t0\t0\t600\t00000000\t0\t0\t0\n");
    oper("wlan0", "up");
    assert!(!check());
    // IPv6 だけの既定の経路。lo の「届かない」印の行は数えない
    v6(V6_LO);
    assert!(!check());
    v6(&format!("{V6_LO}00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000258 00000002 00000000 00000003 wlan0\n"));
    assert!(check());
    oper("wlan0", "lowerlayerdown");
    assert!(!check());
    std::fs::remove_dir_all(&dir).unwrap();

    // 本物の関数は、待たずに答える（結果は環境による）
    let t0 = now_us();
    let _ = (has_default_route(), clock_synchronized());
    assert!(now_us() - t0 < 100_000);
    // この機械自身あての行き先は、経路が無くてもつなぎに行く
    let port = serve_plain(|_, s| echo(s));
    let ep = Endpoint::parse(&format!("localhost:{port}")).unwrap();
    assert!(Dial::start(&ep, &mut NoCredential, None, fast(), None, now_us()).is_ok());
}
