use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;
use typst::layout::Abs;
use typst_as_lib::TypstEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderFormat {
    Markdown,
    Typst,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderMemoInput {
    pub body: String,
    pub format: RenderFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderMemoOutput {
    pub svg: String,
    pub diagnostics: Vec<String>,
    pub elapsed_ms: u128,
    pub cache_key: String,
}

pub fn render_memo(input: RenderMemoInput) -> anyhow::Result<RenderMemoOutput> {
    let started = Instant::now();
    let source = match input.format {
        RenderFormat::Markdown => markdown_source().to_string(),
        RenderFormat::Typst => typst_source(&input.body),
    };
    let mut builder = TypstEngine::builder().with_static_source_file_resolver([("main.typ", source.as_str())]);
    if matches!(input.format, RenderFormat::Markdown) {
        builder = builder.with_static_file_resolver([("memo.md", input.body.as_bytes())]);
    }
    let engine = builder
        .with_package_file_resolver()
        .build();
    let document = engine
        .compile("main.typ")
        .output
        .map_err(|error| anyhow!("Typst compile failed: {error:?}"))?;
    let svg = typst_svg::svg_merged(&document, Abs::pt(0.0));
    Ok(RenderMemoOutput {
        svg,
        diagnostics: Vec::new(),
        elapsed_ms: started.elapsed().as_millis(),
        cache_key: cache_key(&input.body, input.format),
    })
}

fn markdown_source() -> &'static str {
    r##"
#import "@preview/cmarker:0.1.8"
#set page(width: 480pt, height: auto, margin: (x: 22pt, y: 22pt))
#set text(font: ("Noto Serif CJK SC", "Noto Serif SC", "Microsoft YaHei", "New Computer Modern"), size: 12pt, lang: "zh")
#set par(leading: 0.72em, justify: false)
#show raw: it => block(
  fill: rgb("#20261f"),
  radius: 4pt,
  inset: 9pt,
  width: 100%,
  text(font: ("Cascadia Code", "JetBrains Mono", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 9.5pt, fill: rgb("#eaf1e4"), it)
)
#cmarker.render(read("/memo.md"), raw-typst: false)
"##
}

fn typst_source(body: &str) -> String {
    format!(
        r#"
#set page(width: 480pt, height: auto, margin: (x: 22pt, y: 22pt))
#set text(font: ("Noto Serif CJK SC", "Noto Serif SC", "Microsoft YaHei", "New Computer Modern"), size: 12pt, lang: "zh")
#set par(leading: 0.72em, justify: false)
{}
"#,
        body
    )
}

fn cache_key(body: &str, format: RenderFormat) -> String {
    let mut hasher = Sha256::new();
    hasher.update(match format {
        RenderFormat::Markdown => b"markdown".as_slice(),
        RenderFormat::Typst => b"typst".as_slice(),
    });
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_by_format() {
        assert_ne!(
            cache_key("# Hello", RenderFormat::Markdown),
            cache_key("# Hello", RenderFormat::Typst)
        );
    }

    #[test]
    fn typst_smoke_renders_svg() {
        let output = render_memo(RenderMemoInput {
            body: "Hello *Typst*".to_string(),
            format: RenderFormat::Typst,
        })
        .unwrap();
        assert!(output.svg.contains("<svg"));
    }

    #[test]
    #[ignore = "downloads the cmarker Typst package on first run"]
    fn markdown_cmarker_smoke_renders_svg() {
        let output = render_memo(RenderMemoInput {
            body: "# Hello\n\nUse **Markdown**.".to_string(),
            format: RenderFormat::Markdown,
        })
        .unwrap();
        assert!(output.svg.contains("<svg"));
    }
}
