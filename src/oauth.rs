//! Own OAuth2 (loopback) flow to connect the Google account WITHOUT rclone opening the browser
//! or showing its "Success! All done. Please go back to rclone." page.
//!
//! We do the whole dance ourselves: start a local HTTP server, open the browser at Google's
//! consent screen, receive the redirect (showing OUR page), exchange the code for the token,
//! and only then hand rclone the ready-made token with `token=...` (rclone stays just the
//! mount + refresh engine).

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::rclone;

/// How long, at most, we wait for the user to complete the login before giving up.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/drive";

/// The page the user sees in the browser when returning from Google. Our branding, not rclone's.
const SUCCESS_HTML: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>GMount Drive</title><style>html,body{height:100%;margin:0}\
body{font-family:system-ui,sans-serif;background:#1e1e2e;color:#eee;display:flex;\
align-items:center;justify-content:center;text-align:center}.c{max-width:420px;padding:24px}\
h1{color:#8ab4f8;font-size:2.2rem;margin:.2em 0}p{color:#cdd6f4;line-height:1.5}</style></head>\
<body><div class=\"c\"><h1>✅ All set!</h1><p>Your Google account is connected.<br>\
You can close this tab and go back to <b>GMount Drive</b>.</p></div>\
<script>setTimeout(function(){window.close()},800)</script></body></html>";

/// Runs the full OAuth flow with the user's credentials and creates the remote in rclone with the
/// already-obtained token. **BLOCKS** until it completes/fails (run inside spawn_blocking).
///
/// `cancel`: if set to `true` from another thread (e.g. when the wizard window is closed), the
/// wait stops right away and returns `Ok(false)`. `Ok(true)` = account connected.
pub fn connect_with_creds(
    client_id: &str,
    client_secret: &str,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    // 1. Loopback server on a free port. For "Desktop" clients, Google allows any
    //    http://127.0.0.1:<port> as the redirect, without registering it beforehand.
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("couldn't open a local port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let state = gen_state();

    // 2. Open the browser at the consent screen.
    //    access_type=offline + prompt=consent force Google to return a refresh_token.
    let auth_url = format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}\
         &access_type=offline&prompt=consent&state={}",
        urlencode(client_id),
        urlencode(&redirect_uri),
        urlencode(SCOPE),
        urlencode(&state),
    );
    open_url(&auth_url);

    // 3. Wait for the redirect (cancellable / with timeout), serving our success page.
    let code = match wait_for_code(&listener, &state, cancel)? {
        Some(c) => c,
        None => return Ok(false), // the user cancelled (closed the window)
    };

    // 4. Exchange the code for the token against Google.
    let token_json = exchange_code(client_id, client_secret, &code, &redirect_uri)?;

    // 5. Create the remote in rclone with the ready-made token (no browser).
    rclone::create_drive_remote_with_token(client_id, client_secret, &token_json)?;
    Ok(true)
}

/// Waits for a connection on the loopback, replies with the success page and returns `Some(code)`.
/// Returns `Ok(None)` if cancelled (the `cancel` flag). Fails with an error on timeout.
/// Ignores spurious browser requests (e.g. /favicon.ico).
fn wait_for_code(
    listener: &TcpListener,
    expected_state: &str,
    cancel: &AtomicBool,
) -> Result<Option<String>, String> {
    // Non-blocking so we can check cancellation and the timeout between attempts.
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("couldn't configure the local port: {e}"))?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err("login timed out (3 min). Please try again.".to_string());
        }

        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            Err(e) => return Err(format!("error waiting for the browser: {e}")),
        };
        // Blocking mode, but with a read timeout so a stalled/partial client can't hang us
        // forever (cancel and the overall timeout are re-checked back in the accept loop).
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

        // Read until we have the full request line (TCP may split it across segments), capped.
        let mut buf: Vec<u8> = Vec::with_capacity(2048);
        let mut chunk = [0u8; 2048];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(2).any(|w| w == b"\r\n") || buf.len() >= 16384 {
                        break;
                    }
                }
                Err(_) => break, // timeout or error: parse whatever we have
            }
        }
        let req = String::from_utf8_lossy(&buf);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");

        // Requests that aren't the OAuth redirect: 404 and keep waiting.
        if !path.contains("code=") && !path.contains("error=") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }

        // Reply with the nice page before processing.
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            SUCCESS_HTML.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.write_all(SUCCESS_HTML.as_bytes());
        let _ = stream.flush();

        let query = path.splitn(2, '?').nth(1).unwrap_or("");
        let params = parse_query(query);

        if let Some(err) = params.iter().find(|(k, _)| k == "error") {
            return Err(format!("authorization was cancelled or denied ({})", err.1));
        }
        let got_state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v.as_str());
        if got_state != Some(expected_state) {
            return Err("the 'state' doesn't match (possible security issue); please try again".to_string());
        }
        let code = params
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.clone())
            .ok_or_else(|| "Google didn't return the authorization code".to_string())?;
        return Ok(Some(code));
    }
}

/// POSTs to Google's token endpoint and builds the token JSON in the format rclone expects.
fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    let resp = ureq::post(TOKEN_ENDPOINT).send_form(&[
        ("code", code),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ]);

    let body = match resp {
        Ok(r) => r.into_string().map_err(|e| format!("couldn't read the response: {e}"))?,
        Err(ureq::Error::Status(http_code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            return Err(format!("Google rejected the connection (HTTP {http_code}): {detail}"));
        }
        Err(e) => return Err(format!("network error requesting the token: {e}")),
    };

    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("unexpected response from Google: {e}"))?;
    let access = v
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Google didn't return access_token".to_string())?;
    let refresh = v
        .get("refresh_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "Google didn't return a refresh_token. Make sure the app is Published and try again."
                .to_string()
        })?;

    // A zero expiry => rclone treats it as expired and refreshes on first use (then storing a
    // correct expiry). Avoids having to format the date here and is harmless.
    let token = serde_json::json!({
        "access_token": access,
        "token_type": "Bearer",
        "refresh_token": refresh,
        "expiry": "0001-01-01T00:00:00Z",
    });
    Ok(token.to_string())
}

/// Parses a query string `a=b&c=d` into (key, value) pairs with URL-decoding.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = url_decode(it.next().unwrap_or(""));
            let v = url_decode(it.next().unwrap_or(""));
            (k, v)
        })
        .collect()
}

/// RFC3986 percent-encoding: leaves the "unreserved" characters untouched, encodes the rest.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decoding (and `+` -> space) for query values.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Generates an unpredictable `state` for CSRF protection. Uses OS randomness (/dev/urandom) so a
/// local attacker can't guess it and race the loopback redirect; falls back to time+pid.
fn gen_state() -> String {
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut buf = [0u8; 16];
        if f.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, std::process::id())
}

/// Opens a URL in the default browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
