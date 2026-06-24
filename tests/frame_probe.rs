#[path = "../src/error.rs"] mod error;
#[path = "../src/cli.rs"] mod cli;
#[path = "../src/config.rs"] mod config;
#[path = "../src/geometry.rs"] mod geometry;
#[path = "../src/terminal/mod.rs"] mod terminal;
#[path = "../src/browser/mod.rs"] mod browser;
use std::time::Duration;
fn chrome_total_kb() -> i64 {
    let o = std::process::Command::new("bash").arg("-c")
        .arg("ps -axo rss=,command | grep -i 'Google Chrome' | grep -v grep | awk '{s+=$1} END{print s}'")
        .output().ok();
    o.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i64>().ok()).unwrap_or(0)
}
#[tokio::test]
async fn probe() {
    if std::env::var("WEBCAT_ITEST").is_err() { eprintln!("skip"); return; }
    let chrome = browser::profile::discover_chrome(None).unwrap();
    let tmp = std::env::temp_dir().join(format!("wp-{}", std::process::id()));
    let cfg = config::Config { profile_dir: tmp.clone(), chrome: Some(chrome.clone()),
        log_path: tmp.join("log"), quality: 70, dpr: 1.0, start_url: "about:blank".into() };
    let vp = geometry::Viewport { width_px: 1920, height_px: 1080 };
    let (b, mut frames) = browser::Browser::launch(&cfg, chrome, (2200,1400)).await.unwrap();
    b.set_viewport(vp, 1.0).await.unwrap();
    b.navigate("https://www.youtube.com/watch?v=aqz-KE-bpKQ").await.unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let _ = b.eval_string("(function(){var v=document.querySelector('video');if(v){v.muted=true;var p=v.play&&v.play();if(p&&p.catch)p.catch(function(e){});}})()").await;
    // NO screencast — just let the video play and drain nothing
    let _ = &mut frames;
    for i in 0..7 {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let t = b.eval_string("(function(){var v=document.querySelector('video');return v?(''+v.currentTime.toFixed(0)):'na';})()").await.unwrap_or_default();
        eprintln!("+{}s video_t={t}s chrome_total={}KB", (i+1)*10, chrome_total_kb());
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
