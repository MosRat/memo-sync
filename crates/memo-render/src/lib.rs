use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};
use typst::layout::{Abs, PagedDocument};
use typst_as_lib::TypstEngine;

const RENDER_TEMPLATE_VERSION: &[u8] = b"preview-template-v2";

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
    #[serde(default = "default_render_template")]
    pub template: RenderTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenderTemplate {
    Literary,
    Compact,
    Technical,
}

fn default_render_template() -> RenderTemplate {
    RenderTemplate::Literary
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderMemoOutput {
    pub svg: String,
    pub diagnostics: Vec<String>,
    pub elapsed_ms: u128,
    pub cache_key: String,
    pub cached: bool,
    pub width_pt: f64,
    pub height_pt: f64,
    pub pages: Vec<RenderPageOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderPageOutput {
    pub index: usize,
    pub width_pt: f64,
    pub height_pt: f64,
    pub bytes: usize,
    #[serde(skip_serializing, default)]
    pub svg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderMemoMetadata {
    pub diagnostics: Vec<String>,
    pub elapsed_ms: u128,
    pub cache_key: String,
    pub cached: bool,
    pub bytes: usize,
    pub width_pt: f64,
    pub height_pt: f64,
    pub pages: Vec<RenderPageMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderPageMetadata {
    pub index: usize,
    pub width_pt: f64,
    pub height_pt: f64,
    pub bytes: usize,
}

pub fn render_memo(input: RenderMemoInput) -> anyhow::Result<RenderMemoOutput> {
    let cache_key = render_cache_key(&input);
    let started = Instant::now();
    let source = match input.format {
        RenderFormat::Markdown => markdown_source(input.template).to_string(),
        RenderFormat::Typst => typst_source(&input.body, input.template),
    };
    let mut builder =
        TypstEngine::builder().with_static_source_file_resolver([("main.typ", source.as_str())]);
    if matches!(input.format, RenderFormat::Markdown) {
        builder = builder.with_static_file_resolver([("memo.md", input.body.as_bytes())]);
    }
    let engine = builder.with_package_file_resolver().build();
    let document: PagedDocument = engine
        .compile("main.typ")
        .output
        .map_err(|error| anyhow!("Typst compile failed: {error:?}"))?;
    let pages = document
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let svg = typst_svg::svg(page);
            RenderPageOutput {
                index,
                width_pt: page.frame.width().to_pt(),
                height_pt: page.frame.height().to_pt(),
                bytes: svg.len(),
                svg,
            }
        })
        .collect::<Vec<_>>();
    let svg = if pages.len() == 1 {
        pages[0].svg.clone()
    } else {
        typst_svg::svg_merged(&document, Abs::pt(0.0))
    };
    let width_pt = pages.iter().map(|page| page.width_pt).fold(0.0, f64::max);
    let height_pt = pages.iter().map(|page| page.height_pt).sum();
    Ok(RenderMemoOutput {
        svg,
        diagnostics: Vec::new(),
        elapsed_ms: started.elapsed().as_millis(),
        cache_key,
        cached: false,
        width_pt,
        height_pt,
        pages,
    })
}

impl RenderMemoOutput {
    pub fn metadata(&self, cached: bool, elapsed_ms: u128) -> RenderMemoMetadata {
        RenderMemoMetadata {
            diagnostics: self.diagnostics.clone(),
            elapsed_ms,
            cache_key: self.cache_key.clone(),
            cached,
            bytes: self.byte_len(),
            width_pt: self.width_pt,
            height_pt: self.height_pt,
            pages: self
                .pages
                .iter()
                .map(|page| RenderPageMetadata {
                    index: page.index,
                    width_pt: page.width_pt,
                    height_pt: page.height_pt,
                    bytes: page.bytes,
                })
                .collect(),
        }
    }

    pub fn byte_len(&self) -> usize {
        self.svg.len() + self.pages.iter().map(|page| page.bytes).sum::<usize>()
    }
}

#[derive(Debug)]
pub struct RenderCache {
    entries: HashMap<String, RenderMemoOutput>,
    order: VecDeque<String>,
    max_entries: usize,
    max_bytes: usize,
    current_bytes: usize,
}

impl RenderCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1024),
            current_bytes: 0,
        }
    }

    pub fn get(&mut self, key: &str) -> Option<RenderMemoOutput> {
        let cached = self.entries.get(key)?;
        move_to_back(&mut self.order, key);
        let mut output = cached.clone();
        output.cached = true;
        output.elapsed_ms = 0;
        Some(output)
    }

    pub fn get_metadata(&mut self, key: &str) -> Option<RenderMemoMetadata> {
        let cached = self.entries.get(key)?;
        let metadata = cached.metadata(true, 0);
        move_to_back(&mut self.order, key);
        Some(metadata)
    }

    pub fn get_svg(&mut self, key: &str) -> Option<String> {
        let cached = self.entries.get(key)?;
        let svg = cached.svg.clone();
        move_to_back(&mut self.order, key);
        Some(svg)
    }

    pub fn get_page_svg(&mut self, key: &str, index: usize) -> Option<String> {
        let cached = self.entries.get(key)?;
        let svg = cached.pages.get(index)?.svg.clone();
        move_to_back(&mut self.order, key);
        Some(svg)
    }

    pub fn insert(&mut self, output: RenderMemoOutput) -> bool {
        let key = output.cache_key.clone();
        let size = output.byte_len();
        if size > self.max_bytes {
            return false;
        }
        if let Some(existing) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(existing.byte_len());
            self.order.retain(|item| item != &key);
        }
        self.current_bytes += size;
        self.order.push_back(key.clone());
        self.entries.insert(key, output);
        while self.entries.len() > self.max_entries || self.current_bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.current_bytes = self.current_bytes.saturating_sub(removed.byte_len());
            }
        }
        true
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

pub fn render_cache_key(input: &RenderMemoInput) -> String {
    cache_key(&input.body, input.format, input.template)
}

fn move_to_back(order: &mut VecDeque<String>, key: &str) {
    order.retain(|item| item != key);
    order.push_back(key.to_string());
}

fn markdown_source(template: RenderTemplate) -> &'static str {
    match template {
        RenderTemplate::Literary => {
            r##"
#import "@preview/cmarker:0.1.8"
#set page(width: 340pt, height: auto, margin: (x: 18pt, y: 20pt))
#set text(font: ("Noto Serif CJK SC", "Noto Serif SC", "Microsoft YaHei", "New Computer Modern"), size: 14.2pt, lang: "zh", fill: rgb("#211f1b"))
#set par(leading: 0.76em, justify: false, spacing: 0.58em)
#show heading: it => block(above: 0.62em, below: 0.32em, text(weight: 720, fill: rgb("#171512"), it))
#show strong: it => text(weight: 730, fill: rgb("#181612"), it)
#show emph: it => text(style: "italic", fill: rgb("#4b443b"), it)
#show raw: it => block(
  fill: rgb("#20261f"),
  radius: 5pt,
  inset: 10pt,
  width: 100%,
  text(font: ("Cascadia Code", "JetBrains Mono", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 10.4pt, fill: rgb("#eaf1e4"), it)
)
#cmarker.render(read("/memo.md"), raw-typst: false)
"##
        }
        RenderTemplate::Compact => {
            r##"
#import "@preview/cmarker:0.1.8"
#set page(width: 332pt, height: auto, margin: (x: 16pt, y: 16pt))
#set text(font: ("Noto Sans CJK SC", "Microsoft YaHei", "Inter", "New Computer Modern"), size: 12.8pt, lang: "zh", fill: rgb("#27231f"))
#set par(leading: 0.58em, justify: false, spacing: 0.32em)
#show heading: it => block(above: 0.38em, below: 0.18em, text(weight: 720, fill: rgb("#24211d"), it))
#show strong: it => text(weight: 720, fill: rgb("#181612"), it)
#show raw: it => block(
  fill: rgb("#20261f"),
  radius: 4pt,
  inset: 8.5pt,
  width: 100%,
  text(font: ("Cascadia Code", "JetBrains Mono", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 9.7pt, fill: rgb("#eaf1e4"), it)
)
#cmarker.render(read("/memo.md"), raw-typst: false)
"##
        }
        RenderTemplate::Technical => {
            r##"
#import "@preview/cmarker:0.1.8"
#set page(width: 348pt, height: auto, margin: (x: 18pt, y: 18pt))
#set text(font: ("Noto Sans CJK SC", "Microsoft YaHei", "Inter", "New Computer Modern"), size: 13.2pt, lang: "zh", fill: rgb("#20231f"))
#set par(leading: 0.64em, justify: false, spacing: 0.4em)
#show heading: it => block(above: 0.5em, below: 0.24em, text(weight: 740, fill: rgb("#1e2520"), it))
#show strong: it => text(weight: 730, fill: rgb("#181f1a"), it)
#show raw: it => block(
  fill: rgb("#18201b"),
  radius: 4pt,
  inset: 9.5pt,
  width: 100%,
  text(font: ("Cascadia Code", "JetBrains Mono", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 10.2pt, fill: rgb("#dfece2"), it)
)
#cmarker.render(read("/memo.md"), raw-typst: false)
"##
        }
    }
}

fn typst_source(body: &str, template: RenderTemplate) -> String {
    let prelude = match template {
        RenderTemplate::Literary => {
            r##"
#set page(width: 340pt, height: auto, margin: (x: 18pt, y: 20pt))
#set text(font: ("Noto Serif CJK SC", "Noto Serif SC", "Microsoft YaHei", "New Computer Modern"), size: 14.2pt, lang: "zh", fill: rgb("#211f1b"))
#set par(leading: 0.76em, justify: false, spacing: 0.58em)
"##
        }
        RenderTemplate::Compact => {
            r##"
#set page(width: 332pt, height: auto, margin: (x: 16pt, y: 16pt))
#set text(font: ("Noto Sans CJK SC", "Microsoft YaHei", "Inter", "New Computer Modern"), size: 12.8pt, lang: "zh", fill: rgb("#27231f"))
#set par(leading: 0.58em, justify: false, spacing: 0.32em)
"##
        }
        RenderTemplate::Technical => {
            r##"
#set page(width: 348pt, height: auto, margin: (x: 18pt, y: 18pt))
#set text(font: ("Noto Sans CJK SC", "Microsoft YaHei", "Inter", "New Computer Modern"), size: 13.2pt, lang: "zh", fill: rgb("#20231f"))
#set par(leading: 0.64em, justify: false, spacing: 0.4em)
"##
        }
    };
    format!(
        r#"{prelude}
{}
"#,
        body
    )
}

fn cache_key(body: &str, format: RenderFormat, template: RenderTemplate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(RENDER_TEMPLATE_VERSION);
    hasher.update(match format {
        RenderFormat::Markdown => b"markdown".as_slice(),
        RenderFormat::Typst => b"typst".as_slice(),
    });
    hasher.update(match template {
        RenderTemplate::Literary => b"literary".as_slice(),
        RenderTemplate::Compact => b"compact".as_slice(),
        RenderTemplate::Technical => b"technical".as_slice(),
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
            cache_key("# Hello", RenderFormat::Markdown, RenderTemplate::Literary),
            cache_key("# Hello", RenderFormat::Typst, RenderTemplate::Literary)
        );
        assert_ne!(
            cache_key("# Hello", RenderFormat::Markdown, RenderTemplate::Literary),
            cache_key("# Hello", RenderFormat::Markdown, RenderTemplate::Compact)
        );
    }

    #[test]
    fn typst_smoke_renders_svg() {
        let output = render_memo(RenderMemoInput {
            body: "Hello *Typst*".to_string(),
            format: RenderFormat::Typst,
            template: RenderTemplate::Literary,
        })
        .unwrap();
        assert!(output.svg.contains("<svg"));
    }

    #[test]
    fn render_cache_marks_hits_and_evicts() {
        let mut cache = RenderCache::new(1, 1024 * 1024);
        let first = RenderMemoOutput {
            svg: "<svg>one</svg>".to_string(),
            diagnostics: Vec::new(),
            elapsed_ms: 12,
            cache_key: "one".to_string(),
            cached: false,
            width_pt: 1.0,
            height_pt: 1.0,
            pages: Vec::new(),
        };
        let second = RenderMemoOutput {
            svg: "<svg>two</svg>".to_string(),
            diagnostics: Vec::new(),
            elapsed_ms: 10,
            cache_key: "two".to_string(),
            cached: false,
            width_pt: 1.0,
            height_pt: 1.0,
            pages: Vec::new(),
        };
        cache.insert(first);
        assert!(cache.get("one").unwrap().cached);
        cache.insert(second);
        assert_eq!(cache.len(), 1);
        assert!(cache.get("one").is_none());
        assert!(cache.get("two").is_some());
    }

    #[test]
    fn render_cache_serves_metadata_and_page_svg() {
        let mut cache = RenderCache::new(4, 1024 * 1024);
        cache.insert(RenderMemoOutput {
            svg: "<svg>merged</svg>".to_string(),
            diagnostics: Vec::new(),
            elapsed_ms: 9,
            cache_key: "memo".to_string(),
            cached: false,
            width_pt: 100.0,
            height_pt: 80.0,
            pages: vec![RenderPageOutput {
                index: 0,
                width_pt: 100.0,
                height_pt: 80.0,
                bytes: 15,
                svg: "<svg>page</svg>".to_string(),
            }],
        });

        let metadata = cache.get_metadata("memo").unwrap();
        assert!(metadata.cached);
        assert_eq!(metadata.pages.len(), 1);
        assert_eq!(cache.get_page_svg("memo", 0).unwrap(), "<svg>page</svg>");
        assert!(cache.get_page_svg("memo", 1).is_none());
    }

    #[test]
    #[ignore = "downloads the cmarker Typst package on first run"]
    fn markdown_cmarker_smoke_renders_svg() {
        let output = render_memo(RenderMemoInput {
            body: "# Hello\n\nUse **Markdown**.".to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();
        assert!(output.svg.contains("<svg"));
    }
}
