use std::sync::Arc;

use base64::Engine;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::{tool, tool_router};
use serde::Deserialize;

use crate::browser::Browser;
use crate::observability::{ConsolePage, NetworkPage, ObservabilityStore, PageInfo};

pub type CurrentBrowser = Arc<tokio::sync::RwLock<Arc<Browser>>>;

#[derive(Clone)]
pub struct WebcatMcp {
    store: Arc<ObservabilityStore>,
    browser: CurrentBrowser,
    allow_control: bool,
}

impl WebcatMcp {
    pub fn new(
        store: Arc<ObservabilityStore>,
        browser: CurrentBrowser,
        allow_control: bool,
    ) -> Self {
        WebcatMcp {
            store,
            browser,
            allow_control,
        }
    }

    async fn browser(&self) -> Arc<Browser> {
        self.browser.read().await.clone()
    }
}

pub(crate) fn control_guard(allow: bool) -> Result<(), ErrorData> {
    if allow {
        Ok(())
    } else {
        Err(ErrorData::invalid_request(
            "control tools are disabled; start webcat with --mcp-allow-control",
            None,
        ))
    }
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ConsoleQuery {
    pub since_seq: Option<u64>,
    pub level: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NetworkQuery {
    pub since_seq: Option<u64>,
    pub status: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NavigateParam {
    pub url: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ClickParam {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub selector: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct TypeParam {
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct KeyParam {
    pub key: String,
    pub mods: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ScrollParam {
    pub dy: f64,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct EvalParam {
    pub script: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ShotParam {
    pub format: Option<String>,
    pub quality: Option<u8>,
}

fn to_err<E: std::fmt::Display>(e: E) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

fn parse_key(s: &str) -> Result<crate::terminal::keyboard::Key, String> {
    use crate::terminal::keyboard::Key;
    Ok(match s {
        "Enter" => Key::Enter,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Escape" | "Esc" => Key::Esc,
        "ArrowUp" | "Up" => Key::Up,
        "ArrowDown" | "Down" => Key::Down,
        "ArrowLeft" | "Left" => Key::Left,
        "ArrowRight" | "Right" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Delete" => Key::Delete,
        s if s.chars().count() == 1 => Key::Char(s.chars().next().unwrap()),
        _ => return Err(format!("unsupported key: {s}")),
    })
}

fn parse_mods(mods: Option<&[String]>) -> Result<crate::terminal::keyboard::Mods, String> {
    let mut out = crate::terminal::keyboard::Mods::default();
    for m in mods.unwrap_or(&[]) {
        match m.as_str() {
            "Alt" | "alt" => out.alt = true,
            "Ctrl" | "Control" | "ctrl" | "control" => out.ctrl = true,
            "Meta" | "Cmd" | "meta" | "cmd" => out.meta = true,
            "Shift" | "shift" => out.shift = true,
            _ => return Err(format!("unsupported modifier: {m}")),
        }
    }
    Ok(out)
}

#[tool_router(server_handler)]
impl WebcatMcp {
    #[tool(description = "Get page console logs since a sequence number")]
    async fn get_console_logs(&self, Parameters(q): Parameters<ConsoleQuery>) -> Json<ConsolePage> {
        Json(self.store.console_since(
            q.since_seq.unwrap_or(0),
            q.level.as_deref(),
            q.limit.unwrap_or(200),
        ))
    }

    #[tool(description = "Get network request/response logs since a sequence number")]
    async fn get_network_logs(&self, Parameters(q): Parameters<NetworkQuery>) -> Json<NetworkPage> {
        Json(
            self.store
                .network_since(q.since_seq.unwrap_or(0), q.status, q.limit.unwrap_or(200)),
        )
    }

    #[tool(description = "Get current page URL and zoom")]
    async fn get_page_info(&self) -> Json<PageInfo> {
        let browser = self.browser().await;
        Json(browser.page_info(None).await.unwrap_or_else(|_| PageInfo {
            url: String::new(),
            title: String::new(),
            viewport: None,
            loading: false,
        }))
    }

    #[tool(description = "Capture a screenshot of the current page (png or jpeg)")]
    async fn capture_screenshot(
        &self,
        Parameters(p): Parameters<ShotParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let jpeg = p.format.as_deref() == Some("jpeg");
        let browser = self.browser().await;
        let bytes = browser
            .capture_screenshot(jpeg, p.quality.unwrap_or(80))
            .await
            .map_err(to_err)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mime = if jpeg { "image/jpeg" } else { "image/png" };
        Ok(CallToolResult::success(vec![Content::image(
            b64,
            mime.to_string(),
        )]))
    }

    #[tool(description = "Navigate to a URL (requires control)")]
    async fn navigate(
        &self,
        Parameters(p): Parameters<NavigateParam>,
    ) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let browser = self.browser().await;
        browser.navigate(&p.url).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Click at x,y (device px) or a CSS selector (requires control)")]
    async fn click(
        &self,
        Parameters(p): Parameters<ClickParam>,
    ) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let browser = self.browser().await;
        if let Some(sel) = p.selector {
            browser.click_selector(&sel).await.map_err(to_err)?;
        } else if let (Some(x), Some(y)) = (p.x, p.y) {
            browser
                .click(x, y, crate::terminal::mouse::MouseButton::Left)
                .await
                .map_err(to_err)?;
        } else {
            return Err(ErrorData::invalid_request("provide selector or x,y", None));
        }
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Type text into the focused element (requires control)")]
    async fn type_text(
        &self,
        Parameters(p): Parameters<TypeParam>,
    ) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let browser = self.browser().await;
        browser.insert_text(&p.text).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Press a key, optionally with modifiers (requires control)")]
    async fn press_key(
        &self,
        Parameters(p): Parameters<KeyParam>,
    ) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let key = parse_key(&p.key).map_err(|e| ErrorData::invalid_request(e, None))?;
        let mods =
            parse_mods(p.mods.as_deref()).map_err(|e| ErrorData::invalid_request(e, None))?;
        let browser = self.browser().await;
        browser
            .dispatch_key(key, mods, true)
            .await
            .map_err(to_err)?;
        browser
            .dispatch_key(key, mods, false)
            .await
            .map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Scroll by dy at x,y (requires control)")]
    async fn scroll(
        &self,
        Parameters(p): Parameters<ScrollParam>,
    ) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let browser = self.browser().await;
        browser
            .scroll(p.x.unwrap_or(10.0), p.y.unwrap_or(10.0), p.dy)
            .await
            .map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    #[tool(description = "Evaluate JavaScript and return the result string (requires control)")]
    async fn eval_js(
        &self,
        Parameters(p): Parameters<EvalParam>,
    ) -> Result<CallToolResult, ErrorData> {
        control_guard(self.allow_control)?;
        let browser = self.browser().await;
        let out = browser.eval_string(&p.script).await.map_err(to_err)?;
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_disabled_rejects_navigate() {
        assert!(super::control_guard(false).is_err());
        assert!(super::control_guard(true).is_ok());
    }
}
