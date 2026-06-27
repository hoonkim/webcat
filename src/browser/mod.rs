pub mod frame;
pub mod profile;

use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use chromiumoxide::cdp::browser_protocol::emulation::{
    SetDeviceMetricsOverrideParams, SetUserAgentOverrideParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton as CdpMouseButton,
};
use chromiumoxide::cdp::browser_protocol::log::{EnableParams as LogEnableParams, EventEntryAdded};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnableParams, EventLoadingFailed, EventLoadingFinished,
    EventRequestWillBeSent, EventResponseReceived, GetResponseBodyParams, PostDataEntry,
};
use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, CaptureScreenshotFormat, CaptureScreenshotParams,
    EventFrameStartedLoading, EventFrameStoppedLoading, EventJavascriptDialogOpening,
    EventLoadEventFired, EventScreencastFrame, HandleJavaScriptDialogParams, NavigateParams,
    ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EnableParams as RuntimeEnableParams, EventConsoleApiCalled, EventExceptionThrown,
};
use chromiumoxide::{Browser as CdpBrowser, BrowserConfig, Page};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::geometry::Viewport;
use crate::observability::{NetworkEntry, ObservabilityStore, PageInfo, PageViewport};
use crate::terminal::keyboard::{Key, Mods};
use crate::terminal::mouse::MouseButton;
use frame::{frame_channel, Frame, FrameRx, FrameTx};

// Constructed by collect_clickables (called from app.rs); integration test
// includes this file standalone and doesn't call that path.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Clickable {
    pub x: f64,
    pub y: f64,
}

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
    /// True while Chrome reports the main frame as loading.
    #[allow(dead_code)]
    loading_rx: tokio::sync::watch::Receiver<bool>,
    #[allow(dead_code)]
    loading_tx: tokio::sync::watch::Sender<bool>,
    /// Page zoom factor. The frame/grid work in device pixels; Chrome lays the
    /// page out and reports coordinates in CSS pixels (device ÷ zoom), so input
    /// coordinates are divided by zoom and clickable rects multiplied by it.
    zoom: f64,
}

// Methods are called from app.rs; the integration test includes this file
// standalone and only calls a subset, so the rest appear dead there.
#[allow(dead_code)]
impl Browser {
    pub async fn launch(
        cfg: &Config,
        chrome: PathBuf,
        window: (u32, u32),
        store: Arc<ObservabilityStore>,
    ) -> Result<(Browser, FrameRx)> {
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
                let _ = page.execute(SetUserAgentOverrideParams::new(real_ua)).await;
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
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(
                page_shim_js.to_string(),
            ))
            .await;

        let _ = page
            .execute(
                NetworkEnableParams::builder()
                    .max_total_buffer_size(64 * 1024 * 1024)
                    .max_resource_buffer_size(64 * 1024 * 1024)
                    .max_post_data_size(64 * 1024 * 1024)
                    .build(),
            )
            .await;
        let _ = page.execute(RuntimeEnableParams::default()).await;
        let _ = page.execute(LogEnableParams::default()).await;

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
            if let Ok(mut ev) = dialog_page
                .event_listener::<EventJavascriptDialogOpening>()
                .await
            {
                while ev.next().await.is_some() {
                    let _ = dialog_page
                        .execute(HandleJavaScriptDialogParams::new(false))
                        .await;
                }
            }
        });

        let console_page = page.clone();
        let console_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = console_page.event_listener::<EventConsoleApiCalled>().await {
                while let Some(e) = ev.next().await {
                    let args: Vec<serde_json::Value> = e
                        .args
                        .iter()
                        .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
                        .collect();
                    console_store.push_console(
                        e.r#type.as_ref().to_string(),
                        console_args_to_text(&args),
                        None,
                        None,
                    );
                }
            }
        });

        let exc_page = page.clone();
        let exc_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = exc_page.event_listener::<EventExceptionThrown>().await {
                while let Some(e) = ev.next().await {
                    let text = e
                        .exception_details
                        .exception
                        .as_ref()
                        .and_then(|o| o.description.clone())
                        .unwrap_or_else(|| e.exception_details.text.clone());
                    exc_store.push_console("error".into(), text, None, None);
                }
            }
        });

        let log_page = page.clone();
        let log_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = log_page.event_listener::<EventEntryAdded>().await {
                while let Some(e) = ev.next().await {
                    log_store.push_console(
                        e.entry.level.as_ref().to_string(),
                        e.entry.text.clone(),
                        e.entry.url.clone(),
                        e.entry.line_number.map(|n| n as u32),
                    );
                }
            }
        });

        let req_page = page.clone();
        let req_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = req_page.event_listener::<EventRequestWillBeSent>().await {
                while let Some(e) = ev.next().await {
                    req_store.push_network(NetworkEntry {
                        seq: 0,
                        ts_ms: 0,
                        kind: "request".into(),
                        method: Some(e.request.method.clone()),
                        url: e.request.url.clone(),
                        status: None,
                        mime: None,
                        request_id: e.request_id.as_ref().to_string(),
                        request_body: request_body(&e.request.post_data_entries),
                        response_body: None,
                        response_body_base64: None,
                    });
                }
            }
        });

        let resp_page = page.clone();
        let resp_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = resp_page.event_listener::<EventResponseReceived>().await {
                while let Some(e) = ev.next().await {
                    resp_store.push_network(NetworkEntry {
                        seq: 0,
                        ts_ms: 0,
                        kind: "response".into(),
                        method: None,
                        url: e.response.url.clone(),
                        status: Some(e.response.status),
                        mime: Some(e.response.mime_type.clone()),
                        request_id: e.request_id.as_ref().to_string(),
                        request_body: None,
                        response_body: None,
                        response_body_base64: None,
                    });
                }
            }
        });

        let finished_page = page.clone();
        let finished_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = finished_page.event_listener::<EventLoadingFinished>().await {
                while let Some(e) = ev.next().await {
                    let request_id = e.request_id.as_ref().to_string();
                    let Ok(body) = finished_page
                        .execute(GetResponseBodyParams::new(e.request_id.clone()))
                        .await
                    else {
                        continue;
                    };
                    finished_store.attach_response_body(
                        &request_id,
                        body.body.clone(),
                        body.base64_encoded,
                    );
                }
            }
        });

        let fail_page = page.clone();
        let fail_store = store.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = fail_page.event_listener::<EventLoadingFailed>().await {
                while let Some(e) = ev.next().await {
                    fail_store.push_network(NetworkEntry {
                        seq: 0,
                        ts_ms: 0,
                        kind: "failed".into(),
                        method: None,
                        url: String::new(),
                        status: None,
                        mime: None,
                        request_id: e.request_id.as_ref().to_string(),
                        request_body: None,
                        response_body: None,
                        response_body_base64: None,
                    });
                }
            }
        });

        let (loading_tx, loading_rx) = tokio::sync::watch::channel(false);

        let started_page = page.clone();
        let started_loading_tx = loading_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = started_page.event_listener::<EventFrameStartedLoading>().await {
                while ev.next().await.is_some() {
                    let _ = started_loading_tx.send(true);
                }
            }
        });

        let stopped_page = page.clone();
        let stopped_loading_tx = loading_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = stopped_page.event_listener::<EventFrameStoppedLoading>().await {
                while ev.next().await.is_some() {
                    let _ = stopped_loading_tx.send(false);
                }
            }
        });

        // Bump a counter on each page load so the app can re-sync the viewport
        // and screencast after a navigation (a new page is captured at a
        // transitional size otherwise, breaking the fit).
        let (nav_tx, nav_rx) = tokio::sync::watch::channel(0u64);
        let nav_page = page.clone();
        let load_loading_tx = loading_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut ev) = nav_page.event_listener::<EventLoadEventFired>().await {
                let mut n = 0u64;
                while ev.next().await.is_some() {
                    n += 1;
                    let _ = load_loading_tx.send(false);
                    if nav_tx.send(n).is_err() {
                        break;
                    }
                }
            }
        });

        Ok((
            Browser {
                _cdp: cdp,
                page,
                frame_tx,
                alive_rx,
                nav_rx,
                loading_rx,
                loading_tx,
                zoom: cfg.zoom,
            },
            rx,
        ))
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

    pub fn loading(&self) -> tokio::sync::watch::Receiver<bool> {
        self.loading_rx.clone()
    }

    pub fn begin_loading(&self) {
        let _ = self.loading_tx.send(true);
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.begin_loading();
        let before_url = self.current_url().await;
        if let Err(e) = self
            .page
            .execute(NavigateParams::new(url.to_string()))
            .await
        {
            if is_request_timeout(&e) {
                tracing::warn!("navigate response timed out after command dispatch; continuing");
                spawn_loading_watchdog(self.page.clone(), self.loading_tx.clone(), before_url);
                return Ok(());
            }
            let _ = self.loading_tx.send(false);
            return Err(Error::Other(anyhow::anyhow!(e)));
        }
        spawn_loading_watchdog(self.page.clone(), self.loading_tx.clone(), before_url);
        Ok(())
    }

    pub async fn go_back(&self) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.page.evaluate("history.back()"),
        )
        .await;
    }
    pub async fn go_forward(&self) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.page.evaluate("history.forward()"),
        )
        .await;
    }
    pub async fn reload(&self) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.page.reload(),
        )
        .await;
    }

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
            .execute(SetDeviceMetricsOverrideParams::new(
                css_w, css_h, self.zoom, false,
            ))
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

    pub async fn capture_screenshot(&self, jpeg: bool, quality: u8) -> Result<Vec<u8>> {
        let mut b = CaptureScreenshotParams::builder();
        if jpeg {
            b = b
                .format(CaptureScreenshotFormat::Jpeg)
                .quality(quality as i64);
        } else {
            b = b.format(CaptureScreenshotFormat::Png);
        }
        let data = self
            .page
            .execute(b.build())
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?
            .result
            .data;
        let b64: &str = data.as_ref();
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))
    }

    pub async fn page_info(&self, vp: Option<Viewport>) -> Result<PageInfo> {
        let url = self.current_url().await.unwrap_or_default();
        let doc = self
            .eval_string("JSON.stringify({ title: document.title, loading: document.readyState !== 'complete' })")
            .await
            .unwrap_or_else(|_| "{}".into());
        let doc: serde_json::Value = serde_json::from_str(&doc).unwrap_or_default();
        Ok(PageInfo {
            url,
            title: doc
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            viewport: vp.map(|v| PageViewport {
                width_px: v.width_px,
                height_px: v.height_px,
            }),
            loading: doc
                .get("loading")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    pub async fn click_selector(&self, selector: &str) -> Result<()> {
        let js = format!(
            "(() => {{ const el = document.querySelector({sel}); if (!el) return null; \
             const r = el.getBoundingClientRect(); \
             return JSON.stringify([r.left + r.width/2, r.top + r.height/2]); }})()",
            sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into())
        );
        let res = self.eval_string(&js).await?;
        let coords: [f64; 2] = serde_json::from_str(&res)
            .map_err(|_| Error::Other(anyhow::anyhow!("selector not found: {selector}")))?;
        self.click(
            coords[0] * self.zoom,
            coords[1] * self.zoom,
            MouseButton::Left,
        )
        .await
    }

    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.page
            .execute(InsertTextParams::new(text.to_string()))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn dispatch_key(&self, key: Key, mods: Mods, down: bool) -> Result<()> {
        let key_def = key_to_cdp(key);
        let event_type = if down && key_def.text.is_none() && key_def.key.len() > 1 {
            DispatchKeyEventType::RawKeyDown
        } else if down {
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::KeyUp
        };
        let mut p = DispatchKeyEventParams::builder()
            .r#type(event_type)
            .modifiers(encode_mods(mods))
            .key(key_def.key)
            .code(key_def.code)
            .windows_virtual_key_code(key_def.key_code)
            .native_virtual_key_code(key_def.key_code);
        if let Some(t) = key_def.text {
            p = p.text(t);
        }
        let params = p.build().map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page
            .execute(params)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
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
        self.page
            .execute(params)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
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
            self.page
                .execute(params)
                .await
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        }
        Ok(())
    }

    pub async fn scroll(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        // Device → CSS pixels (position and scroll delta both scale by zoom).
        let (x, y, dx, dy) = (
            x / self.zoom,
            y / self.zoom,
            dx / self.zoom,
            dy / self.zoom,
        );
        let params = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseWheel)
            .x(x)
            .y(y)
            .delta_x(dx)
            .delta_y(dy)
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        self.page
            .execute(params)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn current_url(&self) -> Option<String> {
        tokio::time::timeout(std::time::Duration::from_millis(250), self.page.url())
            .await
            .ok()
            .and_then(|res| res.ok().flatten())
    }

    pub async fn eval_string(&self, js: &str) -> Result<String> {
        let v = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.page.evaluate(js),
        )
        .await
        .map_err(|_| Error::Other(anyhow::anyhow!("evaluate timed out")))?
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
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
        Ok(parsed
            .into_iter()
            .map(|(x, y)| Clickable { x: x * z, y: y * z })
            .collect())
    }
}

fn force_device_scale_arg(scale: f64) -> String {
    format!("--force-device-scale-factor={}", scale.clamp(0.5, 4.0))
}

fn console_args_to_text(args: &[serde_json::Value]) -> String {
    args.iter()
        .map(|a| {
            if let Some(v) = a.get("value") {
                match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                }
            } else if let Some(d) = a.get("description").and_then(|d| d.as_str()) {
                d.to_string()
            } else {
                a.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn request_body(entries: &Option<Vec<PostDataEntry>>) -> Option<String> {
    let entries = entries.as_ref()?;
    let mut out = String::new();
    for entry in entries {
        if let Some(bytes) = &entry.bytes {
            out.push_str(&decode_post_data_entry(bytes.as_ref()));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn is_request_timeout<E: std::fmt::Display>(e: &E) -> bool {
    e.to_string().contains("Request timed out")
}

fn spawn_loading_watchdog(
    page: Arc<Page>,
    loading_tx: tokio::sync::watch::Sender<bool>,
    before_url: Option<String>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        for attempt in 0..80 {
            let state = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                page.evaluate(
                    "JSON.stringify({ href: location.href, readyState: document.readyState })",
                ),
            )
            .await
            .ok()
            .and_then(|res| res.ok())
            .and_then(|v| v.into_value::<String>().ok());
            let state: serde_json::Value = state
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let complete = state
                .get("readyState")
                .and_then(|v| v.as_str())
                == Some("complete");
            let href = state.get("href").and_then(|v| v.as_str()).unwrap_or("");
            let moved = before_url.as_deref().is_none_or(|before| before != href);
            if complete && (moved || attempt >= 10) {
                let _ = loading_tx.send(false);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let _ = loading_tx.send(false);
    });
}

fn decode_post_data_entry(data: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| data.to_string())
}

// Called by dispatch_key; appears unused to integration test's standalone compile.
#[allow(dead_code)]
fn encode_mods(m: Mods) -> i64 {
    // CDP modifier bitmask: Alt=1, Ctrl=2, Meta=4, Shift=8.
    let mut bits = 0i64;
    if m.alt {
        bits |= 1;
    }
    if m.ctrl {
        bits |= 2;
    }
    if m.meta {
        bits |= 4;
    }
    if m.shift {
        bits |= 8;
    }
    bits
}

/// Map a Key to (windows virtual key code, optional text to emit).
// Called by dispatch_key; appears unused to integration test's standalone compile.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyDef {
    key: String,
    key_code: i64,
    code: String,
    text: Option<String>,
}

fn key_to_cdp(key: Key) -> KeyDef {
    match key {
        Key::Enter => key_def("Enter", 13, "Enter", Some("\r")),
        Key::Backspace => key_def("Backspace", 8, "Backspace", None),
        Key::Tab => key_def("Tab", 9, "Tab", None),
        Key::Esc => key_def("Escape", 27, "Escape", None),
        Key::Up => key_def("ArrowUp", 38, "ArrowUp", None),
        Key::Down => key_def("ArrowDown", 40, "ArrowDown", None),
        Key::Left => key_def("ArrowLeft", 37, "ArrowLeft", None),
        Key::Right => key_def("ArrowRight", 39, "ArrowRight", None),
        Key::Home => key_def("Home", 36, "Home", None),
        Key::End => key_def("End", 35, "End", None),
        Key::PageUp => key_def("PageUp", 33, "PageUp", None),
        Key::PageDown => key_def("PageDown", 34, "PageDown", None),
        Key::Delete => key_def("Delete", 46, "Delete", None),
        Key::F(n) => key_def(format!("F{n}"), 111 + n as i64, format!("F{n}"), None),
        Key::Char(c) => {
            let code = if c.is_ascii_alphabetic() {
                format!("Key{}", c.to_ascii_uppercase())
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                String::new()
            };
            KeyDef {
                key: c.to_string(),
                key_code: c.to_ascii_uppercase() as i64,
                code,
                text: Some(c.to_string()),
            }
        }
    }
}

fn key_def(
    key: impl Into<String>,
    key_code: i64,
    code: impl Into<String>,
    text: Option<&str>,
) -> KeyDef {
    KeyDef {
        key: key.into(),
        key_code,
        code: code.into(),
        text: text.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        console_args_to_text, force_device_scale_arg, is_request_timeout, key_def, key_to_cdp,
        request_body,
    };
    use base64::Engine;
    use chromiumoxide::cdp::browser_protocol::network::PostDataEntry;
    use crate::terminal::keyboard::Key;
    use serde_json::json;

    #[test]
    fn force_device_scale_arg_clamps_to_supported_range() {
        assert_eq!(force_device_scale_arg(2.0), "--force-device-scale-factor=2");
        assert_eq!(
            force_device_scale_arg(0.1),
            "--force-device-scale-factor=0.5"
        );
        assert_eq!(
            force_device_scale_arg(10.0),
            "--force-device-scale-factor=4"
        );
    }

    #[test]
    fn tab_key_does_not_emit_text_character() {
        assert_eq!(key_to_cdp(Key::Tab), key_def("Tab", 9, "Tab", None));
    }

    #[test]
    fn flattens_string_and_number_args() {
        let args = vec![json!({"value": "hello"}), json!({"value": 42})];
        assert_eq!(console_args_to_text(&args), "hello 42");
    }

    #[test]
    fn falls_back_to_description_when_no_value() {
        let args = vec![json!({"description": "Object"})];
        assert_eq!(console_args_to_text(&args), "Object");
    }

    #[test]
    fn request_body_decodes_cdp_post_data_entries() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            br#"{"operationName":"VerifyPermissions"}"#,
        );
        let entries = Some(vec![PostDataEntry::builder().bytes(encoded).build()]);
        assert_eq!(
            request_body(&entries).as_deref(),
            Some(r#"{"operationName":"VerifyPermissions"}"#)
        );
    }

    #[test]
    fn detects_chromiumoxide_request_timeout() {
        assert!(is_request_timeout(&"Request timed out."));
        assert!(!is_request_timeout(&"navigation failed"));
    }
}
