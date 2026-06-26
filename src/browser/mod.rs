pub mod profile;
pub mod frame;

use std::path::PathBuf;
use std::sync::Arc;
use futures::StreamExt;

use chromiumoxide::{Browser as CdpBrowser, BrowserConfig, Page};
use chromiumoxide::cdp::browser_protocol::page::{
    StartScreencastParams, StartScreencastFormat,
    ScreencastFrameAckParams, EventScreencastFrame,
    NavigateParams, AddScriptToEvaluateOnNewDocumentParams,
    EventJavascriptDialogOpening, HandleJavaScriptDialogParams, EventLoadEventFired,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType,
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton as CdpMouseButton,
    InsertTextParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::{SetDeviceMetricsOverrideParams, SetUserAgentOverrideParams};
use base64::Engine;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::geometry::Viewport;
use crate::terminal::keyboard::{Key, Mods};
use crate::terminal::mouse::MouseButton;
use frame::{Frame, FrameTx, FrameRx, frame_channel};

// Constructed by collect_clickables (called from app.rs); integration test
// includes this file standalone and doesn't call that path.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Clickable { pub x: f64, pub y: f64 }

pub struct Browser {
    // Owning the CdpBrowser keeps the headless Chromium child alive for the
    // controller's lifetime and ensures it is killed when Browser is dropped
    // (satisfies the spec's "clean up Chromium on exit" requirement).
    _cdp: CdpBrowser,
    page: Arc<Page>,
    // Held so the background listener task keeps a strong reference alive;
    // unused directly but must not be dropped.
    #[allow(dead_code)]
    frame_tx: Arc<FrameTx>,
    /// Signals false when the CDP handler task exits (true disconnect).
    // Read via alive() in app.rs; integration test doesn't call alive().
    #[allow(dead_code)]
    alive_rx: tokio::sync::watch::Receiver<bool>,
    /// Increments on each page load so the app can re-sync after navigation.
    #[allow(dead_code)]
    nav_rx: tokio::sync::watch::Receiver<u64>,
    /// Page zoom factor. The frame/grid work in device pixels; Chrome lays the
    /// page out and reports coordinates in CSS pixels (device ÷ zoom), so input
    /// coordinates are divided by zoom and clickable rects multiplied by it.
    zoom: f64,
}

// Methods are called from app.rs; the integration test includes this file
// standalone and only calls a subset, so the rest appear dead there.
#[allow(dead_code)]
impl Browser {
    pub async fn launch(cfg: &Config, chrome: PathBuf, window: (u32, u32)) -> Result<(Browser, FrameRx)> {
        // Use the default profile if free, else a private temp profile, so
        // multiple webcat windows don't deadlock on Chrome's per-profile lock.
        let profile_dir = profile::resolve_profile(&cfg.profile_dir);
        profile::prepare_profile(&profile_dir)?;

        // The screencast captures the compositor WINDOW surface, not just the
        // device-metrics viewport. The window must therefore be at least as
        // large as the page viewport (otherwise the page is cropped to the
        // window). We also force Chrome's compositor scale to match the viewport
        // scale used below; otherwise Retina/macOS Chrome can acknowledge a
        // high-DPI device metrics override while still streaming a logical-size
        // screencast frame, which kitty then has to upscale.
        let bc = BrowserConfig::builder()
            .chrome_executable(chrome)
            .user_data_dir(profile_dir)
            .new_headless_mode()
            .window_size(window.0, window.1)
            .arg("--remote-allow-origins=*")
            .arg("--high-dpi-support=1")
            .arg(force_device_scale_arg(cfg.zoom))
            // Hide the automation signal (navigator.webdriver). Combined with the
            // de-headlessed user agent set below, this lets sites like YouTube
            // that refuse automated/headless clients keep serving media beyond
            // the initial buffer.
            .arg("--disable-blink-features=AutomationControlled")
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;

        let (cdp, mut handler) = CdpBrowser::launch(bc)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;

        // Alive watch channel: starts true and flips false when the CDP handler
        // task exits (i.e., Chrome disconnected).
        let (alive_tx, alive_rx) = tokio::sync::watch::channel(true);

        // Drive the CDP handler in the background; if it ends, the browser is gone.
        // We must NOT break on errors: Chrome 149+ sends CDP events that older
        // chromiumoxide_cdp generated types cannot deserialize, returning transient
        // Err items.  Ignoring those (like the upstream examples do) keeps the
        // handler loop alive.
        tokio::spawn(async move {
            while handler.next().await.is_some() {}
            // Handler loop exited — true disconnect. Signal the alive watch.
            let _ = alive_tx.send(false);
        });

        let page = Arc::new(
            cdp.new_page("about:blank")
                .await
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?,
        );

        // De-headless the user agent: the default headless UA contains
        // "HeadlessChrome", which YouTube (and others) detect and use to cut off
        // media playback after the initial buffer. Take the real UA and drop the
        // "Headless" marker so it matches the installed Chrome version exactly.
        if let Ok(eval) = page.evaluate("navigator.userAgent").await {
            if let Ok(ua) = eval.into_value::<String>() {
                let real_ua = ua.replace("HeadlessChrome", "Chrome");
                let _ = page
                    .execute(SetUserAgentOverrideParams::new(real_ua))
                    .await;
            }
        }

        // Injected on every document (every navigation). Two purposes:
        // 1. Keep navigation in this single captured tab — a new tab (target=
        //    _blank / window.open) becomes a target we don't screencast and our
        //    page is backgrounded, freezing the terminal.
        // 2. Neutralise native browser UI that headless Chrome can't display and
        //    that otherwise blocks the renderer forever (the terminal freezes):
        //    WebAuthn/passkey prompts (navigator.credentials) and permission
        //    requests. These reject/deny immediately so the page falls back
        //    (e.g. to a password form) instead of hanging.
        let page_shim_js = r#"
            (function(){
              try {
                window.open = function(u){ if (u) { try { window.location.href = u; } catch(e){} } return window; };
              } catch(e){}
              document.addEventListener('click', function(e){
                try {
                  var a = e.target && e.target.closest ? e.target.closest('a[target]') : null;
                  if (a && a.target && a.target !== '_self') { a.target = '_self'; }
                } catch(err){}
              }, true);
              try {
                if (navigator.credentials) {
                  var reject = function(){ return Promise.reject(new DOMException('not supported in webcat', 'NotAllowedError')); };
                  navigator.credentials.get = reject;
                  navigator.credentials.create = reject;
                }
              } catch(e){}
              try {
                if (window.Notification) { Notification.requestPermission = function(){ return Promise.resolve('denied'); }; }
              } catch(e){}
            })();
        "#;
        let _ = page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(page_shim_js.to_string()))
            .await;

        let (tx, rx) = frame_channel();
        let frame_tx = Arc::new(tx);

        // Subscribe to screencast frames and forward to the coalescing channel.
        let listener_page = page.clone();
        let listener_tx = frame_tx.clone();
        tokio::spawn(async move {
            let mut events = match listener_page.event_listener::<EventScreencastFrame>().await {
                Ok(s) => s,
                Err(_) => return,
            };
            while let Some(ev) = events.next().await {
                // ev.data is chromiumoxide_types::Binary, which wraps a base64 string.
                // Use AsRef<str> to get the base64 text, then decode.
                let b64: &str = ev.data.as_ref();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .unwrap_or_default();
                listener_tx.send(Frame { jpeg: bytes });
                // Ack so Chromium keeps sending frames.
                let _ = listener_page
                    .execute(ScreencastFrameAckParams::new(ev.session_id))
                    .await;
            }
        });

        // Auto-dismiss JS dialogs (alert/confirm/prompt) so they don't block rendering.
        // HandleJavaScriptDialogParams::new(accept) is the direct constructor (0.7.0).
        let dialog_page = page.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = dialog_page.event_listener::<EventJavascriptDialogOpening>().await {
                while ev.next().await.is_some() {
                    let _ = dialog_page
                        .execute(HandleJavaScriptDialogParams::new(false))
                        .await;
                }
            }
        });

        // Bump a counter on each page load so the app can re-sync the viewport
        // and screencast after a navigation (a new page is captured at a
        // transitional size otherwise, breaking the fit).
        let (nav_tx, nav_rx) = tokio::sync::watch::channel(0u64);
        let nav_page = page.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = nav_page.event_listener::<EventLoadEventFired>().await {
                let mut n = 0u64;
                while ev.next().await.is_some() {
                    n += 1;
                    if nav_tx.send(n).is_err() {
                        break;
                    }
                }
            }
        });

        Ok((Browser { _cdp: cdp, page, frame_tx, alive_rx, nav_rx, zoom: cfg.zoom }, rx))
    }

    /// Returns a watch receiver that starts as `true` and flips to `false`
    /// when the CDP handler task ends (Chrome disconnected or crashed).
    pub fn alive(&self) -> tokio::sync::watch::Receiver<bool> {
        self.alive_rx.clone()
    }

    /// Returns a watch receiver whose value increments each time the page fires
    /// a load event (i.e. a navigation completed).
    pub fn navigated(&self) -> tokio::sync::watch::Receiver<u64> {
        self.nav_rx.clone()
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.page
            .execute(NavigateParams::new(url.to_string()))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn go_back(&self) { let _ = self.page.evaluate("history.back()").await; }
    pub async fn reload(&self) { let _ = self.page.reload().await; }

    /// Set the page metrics for a `dev` capture viewport in DEVICE pixels (what
    /// fills the terminal grid). The CSS layout viewport is `dev / zoom` and the
    /// device scale factor is `zoom`, so the captured frame stays `dev` pixels
    /// while the page lays out (and renders text) at `zoom`× magnification.
    pub async fn set_viewport(&self, dev: Viewport) -> Result<()> {
        // CSS layout viewport = device grid / zoom (natural text size); device
        // scale factor = zoom. The renderer scales the captured frame to the
        // cell grid, which fills the screen.
        let css_w = (dev.width_px as f64 / self.zoom).round().max(1.0) as i64;
        let css_h = (dev.height_px as f64 / self.zoom).round().max(1.0) as i64;
        self.page
            .execute(SetDeviceMetricsOverrideParams::new(css_w, css_h, self.zoom, false))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn start_screencast(&self, quality: u8, vp: Viewport) -> Result<()> {
        // The renderer up-scales the captured (logical-resolution) frame to fill
        // the cell grid. JPEG keeps the stream light and fast.
        let params = StartScreencastParams::builder()
            .format(StartScreencastFormat::Jpeg)
            .quality(quality as i64)
            .max_width(vp.width_px as i64)
            .max_height(vp.height_px as i64)
            .build();
        self.page
            .execute(params)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }


    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.page
            .execute(InsertTextParams::new(text.to_string()))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn dispatch_key(&self, key: Key, mods: Mods, down: bool) -> Result<()> {
        let (vk, text) = key_to_cdp(key);
        let mut p = DispatchKeyEventParams::builder()
            .r#type(if down { DispatchKeyEventType::KeyDown } else { DispatchKeyEventType::KeyUp })
            .modifiers(encode_mods(mods))
            .windows_virtual_key_code(vk);
        if let Some(t) = text {
            p = p.text(t);
        }
        let params = p.build().map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    /// Move the mouse pointer to (x, y) without pressing — drives `:hover`
    /// states and keeps Chrome's hit-test position current.
    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
        let (x, y) = (x / self.zoom, y / self.zoom);
        let params = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseMoved)
            .x(x)
            .y(y)
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn click(&self, x: f64, y: f64, button: MouseButton) -> Result<()> {
        let b = match button {
            MouseButton::Left => CdpMouseButton::Left,
            MouseButton::Middle => CdpMouseButton::Middle,
            MouseButton::Right => CdpMouseButton::Right,
        };
        // `buttons` is the bitmask of buttons currently held: 1=left, 2=right,
        // 4=middle. Chrome requires it to register a real click — pressed sets
        // the bit, released clears it. (Omitting it makes clicks unreliable.)
        let mask: i64 = match button {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
        };
        // Move to the point first so hover-activated targets and hit-testing
        // resolve to the element under the cursor before pressing. move_mouse
        // applies the device→CSS conversion; do the same for press/release here.
        self.move_mouse(x, y).await?;
        let (x, y) = (x / self.zoom, y / self.zoom);
        for (ty, buttons) in [
            (DispatchMouseEventType::MousePressed, mask),
            (DispatchMouseEventType::MouseReleased, 0),
        ] {
            let params = DispatchMouseEventParams::builder()
                .r#type(ty)
                .x(x)
                .y(y)
                .button(b.clone())
                .buttons(buttons)
                .click_count(1i64)
                .build()
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        }
        Ok(())
    }

    pub async fn scroll(&self, x: f64, y: f64, dy: f64) -> Result<()> {
        // Device → CSS pixels (position and scroll delta both scale by zoom).
        let (x, y, dy) = (x / self.zoom, y / self.zoom, dy / self.zoom);
        let params = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseWheel)
            .x(x)
            .y(y)
            .delta_x(0.0f64)
            .delta_y(dy)
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn current_url(&self) -> Option<String> {
        self.page.url().await.ok().flatten()
    }

    pub async fn eval_string(&self, js: &str) -> Result<String> {
        let v = self.page.evaluate(js).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(v.into_value::<String>().unwrap_or_default())
    }

    pub async fn collect_clickables(&self) -> Result<Vec<Clickable>> {
        let js = r#"
            (() => {
              const sel = [
                'a', 'button', 'input', 'textarea', 'select', 'summary',
                '[role=button]', '[role=link]', '[role=menuitem]', '[role=option]',
                '[role=tab]', '[role=checkbox]', '[role=radio]', '[role=row]',
                '[onclick]', '[tabindex]', '[jsaction]', '[data-tooltip]'
              ].join(',');
              const out = [];
              const seen = new Set();
              function isClickable(el) {
                if (!el || el.disabled || el.getAttribute('aria-disabled') === 'true') return false;
                const cs = getComputedStyle(el);
                if (cs.visibility === 'hidden' || cs.display === 'none' || cs.pointerEvents === 'none') return false;
                const name = el.tagName.toLowerCase();
                const role = (el.getAttribute('role') || '').toLowerCase();
                if (['a','button','input','textarea','select','summary'].includes(name)) return true;
                if (['button','link','menuitem','option','tab','checkbox','radio'].includes(role)) return true;
                if (role === 'row' && (el.hasAttribute('jsaction') || cs.cursor === 'pointer')) return true;
                if (el.hasAttribute('onclick') || el.hasAttribute('jsaction')) return true;
                if (el.tabIndex >= 0) return true;
                return cs.cursor === 'pointer';
              }
              for (const el of document.querySelectorAll(sel)) {
                if (!isClickable(el)) continue;
                const r = el.getBoundingClientRect();
                if (r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0
                    && r.top < innerHeight && r.left < innerWidth
                    && r.width < innerWidth * 0.98 && r.height < innerHeight * 0.8) {
                  const x = Math.max(0, Math.min(innerWidth, r.left + r.width / 2));
                  const y = Math.max(0, Math.min(innerHeight, r.top + r.height / 2));
                  const key = Math.round(x / 4) + ':' + Math.round(y / 4);
                  if (!seen.has(key)) {
                    seen.add(key);
                    out.push([x, y]);
                  }
                }
              }
              return JSON.stringify(out);
            })()
        "#;
        let json = self.eval_string(js).await?;
        let parsed: Vec<(f64, f64)> = serde_json::from_str(&json).unwrap_or_default();
        // getBoundingClientRect is in CSS pixels; convert to device pixels (the
        // space the frame and terminal grid use) so hint labels land correctly.
        let z = self.zoom;
        Ok(parsed.into_iter().map(|(x, y)| Clickable { x: x * z, y: y * z }).collect())
    }
}

fn force_device_scale_arg(scale: f64) -> String {
    format!("--force-device-scale-factor={}", scale.clamp(0.5, 4.0))
}

// Called by dispatch_key; appears unused to integration test's standalone compile.
#[allow(dead_code)]
fn encode_mods(m: Mods) -> i64 {
    // CDP modifier bitmask: Alt=1, Ctrl=2, Meta=4, Shift=8.
    let mut bits = 0i64;
    if m.alt   { bits |= 1; }
    if m.ctrl  { bits |= 2; }
    if m.meta  { bits |= 4; }
    if m.shift { bits |= 8; }
    bits
}

/// Map a Key to (windows virtual key code, optional text to emit).
// Called by dispatch_key; appears unused to integration test's standalone compile.
#[allow(dead_code)]
fn key_to_cdp(key: Key) -> (i64, Option<String>) {
    match key {
        Key::Enter     => (13,  Some("\r".into())),
        Key::Backspace => (8,   None),
        Key::Tab       => (9,   Some("\t".into())),
        Key::Esc       => (27,  None),
        Key::Up        => (38,  None),
        Key::Down      => (40,  None),
        Key::Left      => (37,  None),
        Key::Right     => (39,  None),
        Key::Home      => (36,  None),
        Key::End       => (35,  None),
        Key::PageUp    => (33,  None),
        Key::PageDown  => (34,  None),
        Key::Delete    => (46,  None),
        Key::F(n)      => (111 + n as i64, None),
        Key::Char(c)   => (c.to_ascii_uppercase() as i64, Some(c.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::force_device_scale_arg;

    #[test]
    fn force_device_scale_arg_clamps_to_supported_range() {
        assert_eq!(force_device_scale_arg(2.0), "--force-device-scale-factor=2");
        assert_eq!(force_device_scale_arg(0.1), "--force-device-scale-factor=0.5");
        assert_eq!(force_device_scale_arg(10.0), "--force-device-scale-factor=4");
    }
}
