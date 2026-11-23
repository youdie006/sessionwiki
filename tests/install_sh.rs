//! `scripts/install.sh` is the `curl | sh` install from the README.
//!
//! It found the latest tag with `curl ... | grep -m1`. grep exits at the first
//! match, so curl failed writing the rest of the API response and printed
//! "curl: (23) Failure writing output to destination" on an install that
//! worked. The test runs the script with the real curl against a local server
//! that sends a release response larger than a pipe holds.
//!
//! Built only where install.sh has a prebuilt binary; elsewhere it points the
//! user at `cargo install`, and the suite must still pass there.
#![cfg(any(all(target_os = "linux", target_arch = "x86_64"), target_os = "macos"))]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const TAG: &str = "v9.9.9";

/// The target install.sh picks on this machine.
fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        other => unreachable!("the cfg above excludes {other:?}"),
    }
}

fn respond(mut stream: TcpStream, tarball: &[u8]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).unwrap();
    let mut header = String::new();
    while reader.read_line(&mut header).unwrap() > 2 {
        header.clear();
    }
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    // Writes fail once curl gives up on a response; that is what the test
    // looks for on curl's side, not here.
    if path == "/latest" {
        let body = format!(
            "{{\n  \"url\": \"https://api.github.com/repos/youdie006/sessionwiki/releases/1\",\n  \
             \"tag_name\": \"{TAG}\",\n  \"body\": \"{}\"\n}}\n",
            "x".repeat(1 << 20)
        );
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        );
        let _ = stream.write_all(body.as_bytes());
    } else if path.ends_with(".tar.gz") {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            tarball.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(tarball);
    } else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

fn serve(tarball: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            respond(stream, &tarball);
        }
    });
    port
}

fn real_curl() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("curl"))
        .find(|p| p.is_file())
        .expect("curl on PATH")
}

/// A `curl` that sends the installer's GitHub URLs to the local server and
/// passes everything else through to the real curl unchanged.
fn curl_shim(dir: &Path, port: u16) {
    let script = r#"#!/bin/sh
for a; do
  shift
  case "$a" in
    https://api.github.com/repos/youdie006/sessionwiki/releases/latest) a="http://127.0.0.1:PORT/latest" ;;
    https://github.com/youdie006/sessionwiki/releases/download/*) a="http://127.0.0.1:PORT/download/${a##*/}" ;;
    http://*|https://*) echo "test curl: unexpected URL $a" >&2; exit 2 ;;
  esac
  set -- "$@" "$a"
done
exec "REAL" "$@"
"#
    .replace("PORT", &port.to_string())
    .replace("REAL", &real_curl().to_string_lossy());
    let path = dir.join("curl");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn release_tarball(dir: &Path) -> Vec<u8> {
    let name = format!("sessionwiki-{TAG}-{}", target());
    std::fs::create_dir_all(dir.join(&name)).unwrap();
    std::fs::write(
        dir.join(&name).join("sessionwiki"),
        "#!/bin/sh\necho sessionwiki 9.9.9\n",
    )
    .unwrap();
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(dir.join("release.tar.gz"))
        .arg("-C")
        .arg(dir)
        .arg(&name)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(dir.join("release.tar.gz")).unwrap()
}

#[test]
fn a_successful_install_prints_no_curl_error() {
    let t = tempfile::tempdir().unwrap();
    let (shim, pkg, home) = (
        t.path().join("shim"),
        t.path().join("pkg"),
        t.path().join("home"),
    );
    for d in [&shim, &pkg, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let port = serve(release_tarball(&pkg));
    curl_shim(&shim, port);

    let mut path = vec![shim.clone()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let bin = t.path().join("bin");
    let out = std::process::Command::new("sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh"))
        .env_clear()
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HOME", &home)
        .env("SESSIONWIKI_BIN_DIR", &bin)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "install.sh failed: {stderr}");
    assert!(
        stdout.contains(&format!("installed sessionwiki {TAG}")),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(bin.join("sessionwiki")).unwrap(),
        "#!/bin/sh\necho sessionwiki 9.9.9\n"
    );
    assert!(!stderr.contains("curl:"), "{stderr}");
}
