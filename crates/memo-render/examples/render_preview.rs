use std::{env, fs, path::PathBuf};

use memo_render::{render_memo, RenderFormat, RenderMemoInput, RenderTemplate};

fn main() -> anyhow::Result<()> {
    let body = env::args().nth(1).unwrap_or_else(|| {
        "把散落在剪贴板、会议和代码里的句子，收进一个可以同步的地方。\n\nUse **Markdown**, tags, repositories, quick capture, and background sync.\n*italic*\n\n```rust\nfn main() {\n    println!(\"quiet craft, fast notes\");\n}\n```\n\n$ \\frac{1}{2} + \\sin{2} $".to_string()
    });
    let output = render_memo(RenderMemoInput {
        body,
        format: RenderFormat::Markdown,
        template: RenderTemplate::Literary,
    })?;
    let html = format!(
        r#"<!doctype html>
<meta charset="utf-8">
<style>
  body {{
    margin: 0;
    min-height: 100vh;
    display: grid;
    place-items: start center;
    background: #f5efe3;
    font-family: system-ui, sans-serif;
  }}
  .card {{
    width: 560px;
    min-height: 760px;
    margin: 32px;
    padding: 24px;
    border-radius: 12px;
    background: #fffdf8;
    box-shadow: 0 16px 40px rgba(63, 55, 43, 0.14);
  }}
  svg {{
    display: block;
    width: 100%;
    height: auto;
  }}
</style>
<main class="card">
{svg}
</main>
"#,
        svg = output.svg
    );
    let path = env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/render-preview.html"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, html)?;
    println!("{}", path.display());
    Ok(())
}
