pub mod profile;
pub mod frame;

use std::path::PathBuf;
use std::sync::Arc;
use futures::StreamExt;

use chromiumoxide::{Browser as CdpBrowser, BrowserConfig, Page};
use chromiumoxide::cdp::browser_protocol::page::{
    StartScreencastParams, StartScreencastFormat, ScreencastFrameAckParams, EventScreencastFrame,
    NavigateParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType,
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton as CdpMouseButton,
    InsertTextParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use base64::Engine;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::geometry::Viewport;
use crate::terminal::keyboard::{Key, Mods};
use crate::terminal::mouse::MouseButton;
use frame::{Frame, FrameTx, FrameRx, frame_channel};

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
}

impl Browser {
    pub async fn launch(cfg: &Config, chrome: PathBuf) -> Result<(Browser, FrameRx)> {
        profile::prepare_profile(&cfg.profile_dir)?;

        let bc = BrowserConfig::builder()
            .chrome_executable(chrome)
            .user_data_dir(cfg.profile_dir.clone())
            .new_headless_mode()
            .arg("--remote-allow-origins=*")
            .build()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;

        let (cdp, mut handler) = CdpBrowser::launch(bc)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;

        // Drive the CDP handler in the background; if it ends, the browser is gone.
        // We must NOT break on errors: Chrome 149+ sends CDP events that older
        // chromiumoxide_cdp generated types cannot deserialize, returning transient
        // Err items.  Ignoring those (like the upstream examples do) keeps the
        // handler loop alive.
        tokio::spawn(async move {
            while handler.next().await.is_some() {}
        });

        let page = Arc::new(
            cdp.new_page("about:blank")
                .await
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?,
        );

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

        Ok((Browser { _cdp: cdp, page, frame_tx }, rx))
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.page
            .execute(NavigateParams::new(url.to_string()))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn go_back(&self) { let _ = self.page.evaluate("history.back()").await; }
    pub async fn go_forward(&self) { let _ = self.page.evaluate("history.forward()").await; }
    pub async fn reload(&self) { let _ = self.page.reload().await; }

    pub async fn set_viewport(&self, vp: Viewport, dpr: f64) -> Result<()> {
        self.page
            .execute(SetDeviceMetricsOverrideParams::new(
                vp.width_px as i64, vp.height_px as i64, dpr, false,
            ))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        Ok(())
    }

    pub async fn start_screencast(&self, quality: u8, vp: Viewport) -> Result<()> {
        // Note: StartScreencastParams::builder().build() returns StartScreencastParams
        // directly (not Result) in chromiumoxide 0.7.0 — all fields are optional.
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

    pub async fn click(&self, x: f64, y: f64, button: MouseButton) -> Result<()> {
        let b = match button {
            MouseButton::Left => CdpMouseButton::Left,
            MouseButton::Middle => CdpMouseButton::Middle,
            MouseButton::Right => CdpMouseButton::Right,
        };
        for ty in [DispatchMouseEventType::MousePressed, DispatchMouseEventType::MouseReleased] {
            let params = DispatchMouseEventParams::builder()
                .r#type(ty)
                .x(x)
                .y(y)
                .button(b.clone())
                .click_count(1i64)
                .build()
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            self.page.execute(params).await.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        }
        Ok(())
    }

    pub async fn scroll(&self, x: f64, y: f64, dy: f64) -> Result<()> {
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
              const sel = 'a,button,input,textarea,select,[role=button],[onclick]';
              const out = [];
              for (const el of document.querySelectorAll(sel)) {
                const r = el.getBoundingClientRect();
                if (r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0
                    && r.top < innerHeight && r.left < innerWidth) {
                  out.push([r.left + r.width/2, r.top + r.height/2]);
                }
              }
              return JSON.stringify(out);
            })()
        "#;
        let json = self.eval_string(js).await?;
        let parsed: Vec<(f64, f64)> = serde_json::from_str(&json).unwrap_or_default();
        Ok(parsed.into_iter().map(|(x, y)| Clickable { x, y }).collect())
    }
}

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
