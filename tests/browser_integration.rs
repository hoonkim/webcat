// Run with: WEBCAT_ITEST=1 cargo test --test browser_integration -- --nocapture
// Skipped unless WEBCAT_ITEST=1 and a chrome binary is discoverable.

#[path = "../src/browser/mod.rs"]
mod browser;
#[path = "../src/cli.rs"]
mod cli;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/geometry.rs"]
mod geometry;
#[path = "../src/observability.rs"]
mod observability;
#[path = "../src/terminal/mod.rs"]
mod terminal;

use std::time::Duration;
use terminal::keyboard::{Key, Mods};

fn itest_enabled() -> bool {
    std::env::var("WEBCAT_ITEST").is_ok()
}

fn store() -> std::sync::Arc<observability::ObservabilityStore> {
    std::sync::Arc::new(observability::ObservabilityStore::new(2000))
}

#[tokio::test]
async fn navigate_and_screencast_and_korean_input() {
    if !itest_enabled() {
        eprintln!("skipped (set WEBCAT_ITEST=1)");
        return;
    }

    let chrome = browser::profile::discover_chrome(None).expect("chrome");
    let tmp = std::env::temp_dir().join(format!("webcat-itest-{}", std::process::id()));
    let cfg = config::Config {
        profile_dir: tmp.clone(),
        chrome: Some(chrome.clone()),
        log_path: tmp.join("log"),
        quality: 70,
        zoom: 1.0,
        start_url: "about:blank".into(),
        mcp: config::McpConfig {
            enabled: false,
            port: None,
            allow_control: false,
        },
    };

    let (b, mut frames) = browser::Browser::launch(&cfg, chrome, (1024, 856), store())
        .await
        .expect("launch");
    let vp = geometry::Viewport {
        width_px: 800,
        height_px: 600,
    };
    b.set_viewport(vp).await.unwrap();

    // Page with a text input we can focus and type Korean into.
    let html = "data:text/html,<input id=t autofocus>";
    b.navigate(html).await.unwrap();

    b.start_screencast(70, vp).await.unwrap();
    // We should receive at least one frame within a few seconds.
    let got = tokio::time::timeout(Duration::from_secs(5), frames.recv()).await;
    assert!(matches!(got, Ok(Some(_))), "expected a screencast frame");

    // Korean round-trip via insertText.
    b.insert_text("안녕하세요").await.unwrap();
    let value = b
        .eval_string("document.getElementById('t').value")
        .await
        .unwrap();
    assert_eq!(value, "안녕하세요");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn collect_clickables_includes_gmail_style_rows() {
    if !itest_enabled() {
        eprintln!("skipped (set WEBCAT_ITEST=1)");
        return;
    }

    let chrome = browser::profile::discover_chrome(None).expect("chrome");
    let tmp = std::env::temp_dir().join(format!("webcat-clickables-itest-{}", std::process::id()));
    let cfg = config::Config {
        profile_dir: tmp.clone(),
        chrome: Some(chrome.clone()),
        log_path: tmp.join("log"),
        quality: 70,
        zoom: 1.0,
        start_url: "about:blank".into(),
        mcp: config::McpConfig {
            enabled: false,
            port: None,
            allow_control: false,
        },
    };

    let (b, _frames) = browser::Browser::launch(&cfg, chrome, (1024, 856), store())
        .await
        .expect("launch");
    b.set_viewport(geometry::Viewport {
        width_px: 800,
        height_px: 600,
    })
    .await
    .unwrap();

    let html = r#"data:text/html,
      <div role='grid'>
        <div role='row' jsaction='click:openThread' tabindex='-1'
             style='position:absolute;left:80px;top:120px;width:640px;height:42px'>
          <span>Quarterly update subject</span>
        </div>
      </div>
    "#;
    b.navigate(html).await.unwrap();

    let clickables = b.collect_clickables().await.unwrap();
    assert!(
        clickables
            .iter()
            .any(|c| c.x > 300.0 && c.x < 520.0 && c.y > 130.0 && c.y < 160.0),
        "expected Gmail-style row in clickables: {clickables:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn tab_key_moves_focus_between_page_controls() {
    if !itest_enabled() {
        eprintln!("skipped (set WEBCAT_ITEST=1)");
        return;
    }

    let chrome = browser::profile::discover_chrome(None).expect("chrome");
    let tmp = std::env::temp_dir().join(format!("webcat-tab-itest-{}", std::process::id()));
    let cfg = config::Config {
        profile_dir: tmp.clone(),
        chrome: Some(chrome.clone()),
        log_path: tmp.join("log"),
        quality: 70,
        zoom: 1.0,
        start_url: "about:blank".into(),
        mcp: config::McpConfig {
            enabled: false,
            port: None,
            allow_control: false,
        },
    };

    let (b, _frames) = browser::Browser::launch(&cfg, chrome, (1024, 856), store())
        .await
        .expect("launch");
    b.set_viewport(geometry::Viewport {
        width_px: 800,
        height_px: 600,
    })
    .await
    .unwrap();

    let html = "data:text/html,<button id=a autofocus>A</button><button id=b>B</button>";
    b.navigate(html).await.unwrap();
    b.eval_string("document.getElementById('a').focus(); document.activeElement.id")
        .await
        .unwrap();

    b.dispatch_key(Key::Tab, Mods::none(), true).await.unwrap();
    b.dispatch_key(Key::Tab, Mods::none(), false).await.unwrap();

    let active = b.eval_string("document.activeElement.id").await.unwrap();
    assert_eq!(active, "b");

    let _ = std::fs::remove_dir_all(&tmp);
}
