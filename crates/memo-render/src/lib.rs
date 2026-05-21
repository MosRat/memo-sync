use anyhow::anyhow;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::LazyLock,
    time::Instant,
};
use typst::foundations::{Dict, IntoValue};
use typst::layout::{Abs, PagedDocument};
use typst_as_lib::{typst_kit_options::TypstKitFontOptions, TypstEngine, TypstTemplateMainFile};

const RENDER_TEMPLATE_VERSION: &[u8] = b"preview-template-v17";
const RENDER_MAIN_TEMPLATE: &str = r#"#import sys: inputs
#eval(inputs.source, mode: "markup")
"#;
const PREVIEW_SERIF_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSerifSC-VF.ttf");
const PREVIEW_SANS_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSansSC-VF.ttf");
const PREVIEW_MONO_FONT: &[u8] = include_bytes!("../assets/fonts/CascadiaCode.ttf");
const PREVIEW_INTER_FONT: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");
const PREVIEW_JETBRAINS_MONO_FONT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-VF.ttf");
const PREVIEW_WENKAI_FONT: &[u8] = include_bytes!("../assets/fonts/LXGWWenKai-Regular.ttf");

static RENDER_ENGINE: LazyLock<TypstEngine<TypstTemplateMainFile>> = LazyLock::new(|| {
    TypstEngine::builder()
        .fonts([
            PREVIEW_SERIF_FONT,
            PREVIEW_SANS_FONT,
            PREVIEW_MONO_FONT,
            PREVIEW_INTER_FONT,
            PREVIEW_JETBRAINS_MONO_FONT,
            PREVIEW_WENKAI_FONT,
        ])
        .search_fonts_with(
            TypstKitFontOptions::default()
                .include_system_fonts(false)
                .include_embedded_fonts(true),
        )
        .main_file(RENDER_MAIN_TEMPLATE)
        .with_package_file_resolver()
        .build()
});

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
    Magazine,
    Notebook,
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
    let expects_text = input.format == RenderFormat::Markdown && markdown_expects_text(&input.body);
    let (source, diagnostics) = match input.format {
        RenderFormat::Markdown => {
            let (typst_body, diagnostics) = markdown_to_typst(&input.body);
            (typst_source(&typst_body, input.template), diagnostics)
        }
        RenderFormat::Typst => (typst_source(&input.body, input.template), Vec::new()),
    };
    let mut inputs = Dict::new();
    inputs.insert("source".into(), source.into_value());
    let document: PagedDocument = RENDER_ENGINE
        .compile_with_input(inputs)
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
    if expects_text && !svg_has_text_geometry(&svg) {
        return Err(anyhow!(
            "Typst generated no visible text glyphs; check preview font availability"
        ));
    }
    let width_pt = pages.iter().map(|page| page.width_pt).fold(0.0, f64::max);
    let height_pt = pages.iter().map(|page| page.height_pt).sum();
    Ok(RenderMemoOutput {
        svg,
        diagnostics,
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

fn typst_source(body: &str, template: RenderTemplate) -> String {
    let prelude = match template {
        RenderTemplate::Literary => {
            r##"
#set page(width: 320pt, height: auto, margin: (x: 14pt, y: 9pt), fill: none)
#set text(font: ("Noto Serif CJK SC", "Noto Serif SC", "Inter", "Microsoft YaHei", "New Computer Modern"), size: 13.2pt, lang: "zh", fill: rgb("#211f1b"))
#set par(leading: 0.7em, justify: false, spacing: 0.42em)
#set math.equation(numbering: none)
#set list(indent: 1.08em, body-indent: 0.42em)
#set enum(indent: 1.2em, body-indent: 0.48em)
#show heading.where(level: 1): it => block(above: 0.08em, below: 0.08em, text(size: 1.16em, weight: 760, fill: rgb("#171512"), it))
#show heading.where(level: 2): it => block(above: 0.26em, below: 0.06em, text(size: 1.03em, weight: 730, fill: rgb("#2c2520"), it))
#show heading: it => block(above: 0.22em, below: 0.04em, text(size: 0.97em, weight: 720, fill: rgb("#352d27"), it))
#show strong: it => text(weight: 760, fill: rgb("#171512"), it)
#show emph: it => text(style: "italic", fill: rgb("#735742"), it)
#show link: it => text(fill: rgb("#7b563d"), underline(it))
#show quote: it => block(stroke: (left: 1.6pt + rgb("#c9ad8a")), inset: (left: 7pt), above: 0.22em, below: 0.16em, text(fill: rgb("#5f5147"), it))
#show raw.where(block: false): it => box(
  fill: rgb("#eee7da"),
  radius: 2.4pt,
  inset: (x: 2.8pt, y: 1.1pt),
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 0.86em, fill: rgb("#6b3f31"), it)
)
#show raw.where(block: true): it => block(
  fill: rgb("#1f261f"),
  radius: 4pt,
  inset: 6.8pt,
  above: 0.56em,
  below: 0.52em,
  width: 100%,
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 8.9pt, fill: rgb("#eaf1e4"), it)
)
"##
        }
        RenderTemplate::Compact => {
            r##"
#set page(width: 304pt, height: auto, margin: (x: 11pt, y: 8pt), fill: none)
#set text(font: ("Inter", "Noto Sans CJK SC", "Microsoft YaHei", "New Computer Modern"), size: 12.2pt, lang: "zh", fill: rgb("#27231f"))
#set par(leading: 0.6em, justify: false, spacing: 0.28em)
#set math.equation(numbering: none)
#set list(indent: 1em, body-indent: 0.34em)
#set enum(indent: 1.1em, body-indent: 0.42em)
#show heading.where(level: 1): it => block(above: 0.04em, below: 0.06em, text(size: 1.1em, weight: 760, fill: rgb("#211e1a"), it))
#show heading.where(level: 2): it => block(above: 0.2em, below: 0.04em, text(size: 1em, weight: 730, fill: rgb("#2e2a25"), it))
#show heading: it => block(above: 0.16em, below: 0.04em, text(size: 0.95em, weight: 710, fill: rgb("#36312b"), it))
#show strong: it => text(weight: 760, fill: rgb("#1d1b18"), it)
#show emph: it => text(style: "italic", fill: rgb("#665a4d"), it)
#show link: it => text(fill: rgb("#596f62"), underline(it))
#show quote: it => block(stroke: (left: 1.4pt + rgb("#b9b0a1")), inset: (left: 6pt), above: 0.16em, below: 0.12em, text(fill: rgb("#5d574f"), it))
#show raw.where(block: false): it => box(
  fill: rgb("#ece8df"),
  radius: 2pt,
  inset: (x: 2.6pt, y: 0.8pt),
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 0.84em, fill: rgb("#5b4338"), it)
)
#show raw.where(block: true): it => block(
  fill: rgb("#20261f"),
  radius: 4pt,
  inset: 6pt,
  above: 0.46em,
  below: 0.4em,
  width: 100%,
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 8.75pt, fill: rgb("#eaf1e4"), it)
)
"##
        }
        RenderTemplate::Technical => {
            r##"
#set page(width: 328pt, height: auto, margin: (x: 13pt, y: 10pt), fill: none)
#set text(font: ("Inter", "Noto Sans CJK SC", "Microsoft YaHei", "New Computer Modern"), size: 12.6pt, lang: "zh", fill: rgb("#20231f"))
#set par(leading: 0.62em, justify: false, spacing: 0.28em)
#set math.equation(numbering: none)
#set list(indent: 1.05em, body-indent: 0.38em)
#set enum(indent: 1.16em, body-indent: 0.44em)
#show heading.where(level: 1): it => block(above: 0.04em, below: 0.06em, text(size: 1.1em, weight: 780, fill: rgb("#162019"), it))
#show heading.where(level: 2): it => block(above: 0.22em, below: 0.04em, text(size: 1em, weight: 760, fill: rgb("#1e2b23"), it))
#show heading: it => block(above: 0.16em, below: 0.04em, text(size: 0.95em, weight: 730, fill: rgb("#24332b"), it))
#show strong: it => text(weight: 780, fill: rgb("#18221b"), it)
#show emph: it => text(style: "italic", fill: rgb("#526553"), it)
#show link: it => text(fill: rgb("#2f7462"), underline(it))
#show quote: it => block(fill: rgb("#edf1ec"), stroke: (left: 1.6pt + rgb("#819b88")), inset: (x: 7pt, y: 3pt), radius: 3pt, above: 0.18em, below: 0.12em, text(fill: rgb("#4a5c4f"), it))
#show raw.where(block: false): it => box(
  fill: rgb("#e9eee9"),
  radius: 2pt,
  inset: (x: 2.6pt, y: 0.9pt),
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 0.84em, fill: rgb("#235341"), it)
)
#show raw.where(block: true): it => block(
  fill: rgb("#151d19"),
  radius: 4pt,
  inset: 6.6pt,
  above: 0.48em,
  below: 0.42em,
  width: 100%,
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 9.1pt, fill: rgb("#dfece2"), it)
)
"##
        }
        RenderTemplate::Magazine => {
            r##"
#set page(width: 334pt, height: auto, margin: (x: 15pt, y: 11pt), fill: none)
#set text(font: ("Noto Serif CJK SC", "Noto Serif SC", "Inter", "Georgia", "New Computer Modern"), size: 13.6pt, lang: "zh", fill: rgb("#201b16"))
#set par(leading: 0.72em, justify: false, spacing: 0.4em)
#set math.equation(numbering: none)
#set list(indent: 1.1em, body-indent: 0.44em)
#set enum(indent: 1.22em, body-indent: 0.5em)
#show heading.where(level: 1): it => block(above: 0.04em, below: 0.08em, text(size: 1.2em, weight: 780, fill: rgb("#15120f"), it))
#show heading.where(level: 2): it => block(above: 0.26em, below: 0.06em, text(size: 1.04em, weight: 740, fill: rgb("#2b2119"), it))
#show heading: it => block(above: 0.2em, below: 0.04em, text(size: 0.96em, weight: 730, fill: rgb("#3a2b20"), it))
#show strong: it => text(weight: 780, fill: rgb("#18120e"), it)
#show emph: it => text(style: "italic", fill: rgb("#755b45"), it)
#show link: it => text(fill: rgb("#9a5a3d"), underline(it))
#show quote: it => block(fill: rgb("#f2ebe0"), stroke: (left: 1.8pt + rgb("#d0825f")), inset: (x: 7pt, y: 3pt), radius: 3pt, above: 0.22em, below: 0.16em, text(fill: rgb("#654a39"), it))
#show raw.where(block: false): it => box(
  fill: rgb("#efe6d9"),
  radius: 2.4pt,
  inset: (x: 2.8pt, y: 1pt),
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 0.84em, fill: rgb("#794433"), it)
)
#show raw.where(block: true): it => block(
  fill: rgb("#211d19"),
  radius: 5pt,
  inset: 7pt,
  above: 0.56em,
  below: 0.52em,
  width: 100%,
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 9pt, fill: rgb("#f6efe4"), it)
)
"##
        }
        RenderTemplate::Notebook => {
            r##"
#set page(width: 318pt, height: auto, margin: (x: 13pt, y: 8pt), fill: none)
#set text(font: ("LXGW WenKai", "Inter", "Noto Sans CJK SC", "Microsoft YaHei", "New Computer Modern"), size: 12.8pt, lang: "zh", fill: rgb("#282521"))
#set par(leading: 0.64em, justify: false, spacing: 0.3em)
#set math.equation(numbering: none)
#set list(indent: 1.08em, body-indent: 0.4em)
#set enum(indent: 1.16em, body-indent: 0.46em)
#show heading.where(level: 1): it => block(above: 0.04em, below: 0.06em, text(size: 1.12em, weight: 760, fill: rgb("#22201d"), it))
#show heading.where(level: 2): it => block(above: 0.22em, below: 0.04em, text(size: 1em, weight: 730, fill: rgb("#2d2924"), it))
#show heading: it => block(above: 0.16em, below: 0.04em, text(size: 0.96em, weight: 720, fill: rgb("#37322b"), it))
#show strong: it => text(weight: 760, fill: rgb("#201d19"), it)
#show emph: it => text(style: "italic", fill: rgb("#7b5948"), it)
#show link: it => text(fill: rgb("#6b7457"), underline(it))
#show quote: it => block(fill: rgb("#f1eee5"), stroke: (left: 1.7pt + rgb("#c86f52")), inset: (x: 7pt, y: 3pt), radius: 3pt, above: 0.2em, below: 0.14em, text(fill: rgb("#675a50"), it))
#show raw.where(block: false): it => box(
  fill: rgb("#ebe6d9"),
  radius: 2pt,
  inset: (x: 2.6pt, y: 0.9pt),
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 0.84em, fill: rgb("#684135"), it)
)
#show raw.where(block: true): it => block(
  fill: rgb("#1d2722"),
  radius: 4pt,
  inset: 6pt,
  above: 0.48em,
  below: 0.42em,
  width: 100%,
  text(font: ("JetBrains Mono", "Cascadia Code", "Noto Sans Mono CJK SC", "DejaVu Sans Mono"), size: 8.6pt, fill: rgb("#e9f2ea"), it)
)
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

fn markdown_to_typst(markdown: &str) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut diagnostics = Vec::new();
    if has_unclosed_markdown_fence(markdown) {
        diagnostics.push("Closed an unfinished Markdown code fence for preview.".to_string());
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let mut code_block = false;
    let mut list_stack: Vec<bool> = Vec::new();
    let mut item_depth = 0usize;
    let mut table_depth = 0usize;
    let mut table_cell_open = false;
    let mut paragraph_stack: Vec<bool> = Vec::new();

    let normalized_markdown = normalize_markdown_math(markdown);
    for event in Parser::new_ext(&normalized_markdown, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    let wrap = item_depth == 0 && table_depth == 0;
                    paragraph_stack.push(wrap);
                    if wrap {
                        out.push_str("#block[");
                    }
                }
                Tag::Heading { level, .. } => {
                    out.push_str(&"=".repeat(heading_level_number(level)));
                    out.push(' ');
                }
                Tag::BlockQuote(_) => out.push_str("#quote(block: true)["),
                Tag::CodeBlock(kind) => {
                    code_block = true;
                    out.push_str("#v(0.22em)\n");
                    out.push_str("```");
                    if let CodeBlockKind::Fenced(language) = kind {
                        out.push_str(language.trim());
                    }
                    out.push('\n');
                }
                Tag::List(start) => list_stack.push(start.is_some()),
                Tag::Item => {
                    item_depth += 1;
                    out.push_str(if *list_stack.last().unwrap_or(&false) {
                        "+ "
                    } else {
                        "- "
                    });
                }
                Tag::Emphasis => out.push_str("#emph["),
                Tag::Strong => out.push_str("#strong["),
                Tag::Strikethrough => out.push_str("#strike["),
                Tag::Superscript => out.push_str("#super["),
                Tag::Subscript => out.push_str("#sub["),
                Tag::Link { dest_url, .. } => {
                    out.push_str("#link(\"");
                    out.push_str(&escape_typst_string(&dest_url));
                    out.push_str("\")[");
                }
                Tag::Image { dest_url, .. } => {
                    out.push_str("#emph[image: ");
                    out.push_str(&escape_typst_text(&dest_url));
                    out.push_str(" / ");
                }
                Tag::Table(_) => {
                    table_depth += 1;
                    out.push_str("#block(fill: rgb(\"#f5f0e7\"), radius: 3pt, inset: 5pt)[");
                }
                Tag::TableHead | Tag::TableRow => {}
                Tag::TableCell => {
                    if table_cell_open {
                        out.push_str(" | ");
                    }
                    table_cell_open = true;
                }
                Tag::HtmlBlock
                | Tag::FootnoteDefinition(_)
                | Tag::DefinitionList
                | Tag::DefinitionListTitle
                | Tag::DefinitionListDefinition
                | Tag::MetadataBlock(_) => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if paragraph_stack.pop().unwrap_or(false) {
                        out.push_str("]\n#v(0.82em)\n");
                    } else if item_depth == 0 {
                        out.push_str("\n\n");
                    }
                }
                TagEnd::Heading(level) => {
                    out.push('\n');
                    out.push_str(heading_after_spacing(level));
                    out.push('\n');
                }
                TagEnd::BlockQuote(_) => out.push_str("]\n\n"),
                TagEnd::CodeBlock => {
                    code_block = false;
                    out.push_str("```\n#v(0.72em)\n\n");
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    out.push('\n');
                }
                TagEnd::Item => {
                    item_depth = item_depth.saturating_sub(1);
                    out.push('\n');
                }
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript
                | TagEnd::Link
                | TagEnd::Image => out.push(']'),
                TagEnd::Table => {
                    table_depth = table_depth.saturating_sub(1);
                    table_cell_open = false;
                    out.push_str("]\n\n");
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    table_cell_open = false;
                    out.push_str("\\\n");
                }
                TagEnd::TableCell => {}
                TagEnd::HtmlBlock
                | TagEnd::FootnoteDefinition
                | TagEnd::DefinitionList
                | TagEnd::DefinitionListTitle
                | TagEnd::DefinitionListDefinition
                | TagEnd::MetadataBlock(_) => {}
            },
            Event::Text(text) => {
                if code_block {
                    out.push_str(&text);
                } else {
                    out.push_str(&escape_typst_text(&text));
                }
            }
            Event::Code(code) => {
                out.push_str("#raw(\"");
                out.push_str(&escape_typst_string(&code));
                out.push_str("\")");
            }
            Event::InlineMath(math) => push_typst_math(&mut out, &math, false),
            Event::DisplayMath(math) => push_typst_math(&mut out, &math, true),
            Event::Html(html) | Event::InlineHtml(html) => {
                if !html.trim().is_empty() {
                    diagnostics.push("Ignored raw HTML in Markdown preview.".to_string());
                }
            }
            Event::FootnoteReference(label) => {
                out.push_str("#super[");
                out.push_str(&escape_typst_text(&label));
                out.push(']');
            }
            Event::SoftBreak => out.push_str("\\\n"),
            Event::HardBreak => out.push_str("\\\n"),
            Event::Rule => {
                out.push_str("#line(length: 100%, stroke: 0.7pt + rgb(\"#d5cab8\"))\n\n")
            }
            Event::TaskListMarker(checked) => out.push_str(if checked { "[x] " } else { "[ ] " }),
        }
    }

    (out, diagnostics)
}

fn has_unclosed_markdown_fence(markdown: &str) -> bool {
    let mut fence: Option<&str> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(current) = fence {
            if trimmed.starts_with(current) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
        } else if trimmed.starts_with("~~~") {
            fence = Some("~~~");
        }
    }
    fence.is_some()
}

fn normalize_markdown_math(markdown: &str) -> String {
    let markdown = promote_standalone_math_lines(markdown);
    let mut output = String::with_capacity(markdown.len());
    let mut rest = markdown.as_str();
    while let Some(start) = rest.find('$') {
        output.push_str(&rest[..start]);
        if let Some(after_open) = rest[start..].strip_prefix("$$") {
            if let Some(end) = after_open.find("$$") {
                output.push_str("$$");
                output.push_str(after_open[..end].trim());
                output.push_str("$$");
                rest = &after_open[end + 2..];
                continue;
            }
        } else {
            let after_open = &rest[start + 1..];
            if let Some(end) = after_open.find('$') {
                output.push('$');
                output.push_str(after_open[..end].trim());
                output.push('$');
                rest = &after_open[end + 1..];
                continue;
            }
        }
        output.push('$');
        rest = &rest[start + 1..];
    }
    output.push_str(rest);
    output
}

fn promote_standalone_math_lines(markdown: &str) -> String {
    let mut output = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.len() > 2
            && trimmed.starts_with('$')
            && trimmed.ends_with('$')
            && !trimmed.starts_with("$$")
            && !trimmed.ends_with("$$")
        {
            output.push_str("$$");
            output.push_str(trimmed.trim_matches('$').trim());
            output.push_str("$$\n");
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !markdown.ends_with('\n') {
        output.pop();
    }
    output
}

fn heading_level_number(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn heading_after_spacing(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "#v(0.92em)",
        HeadingLevel::H2 => "#v(0.78em)",
        HeadingLevel::H3 => "#v(0.66em)",
        HeadingLevel::H4 => "#v(0.54em)",
        HeadingLevel::H5 | HeadingLevel::H6 => "#v(0.48em)",
    }
}

fn push_typst_math(out: &mut String, math: &str, display: bool) {
    if display {
        out.push_str("#block(above: 0.36em, below: 0.28em)[$");
        out.push_str(&latex_math_to_typst(math.trim()));
        out.push_str("$]\n\n");
    } else {
        out.push('$');
        out.push_str(&latex_math_to_typst(math.trim()));
        out.push('$');
    }
}

fn latex_math_to_typst(math: &str) -> String {
    let mut converted = convert_latex_frac(math);
    for name in ["sin", "cos", "tan", "log", "ln", "sqrt"] {
        converted = converted.replace(&format!("\\{name}{{"), &format!("{name}("));
    }
    converted = converted.replace('}', ")");
    converted.replace('\\', "")
}

fn convert_latex_frac(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("\\frac{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + "\\frac".len()..];
        if let Some((numerator, after_numerator)) = take_braced(after) {
            if let Some((denominator, after_denominator)) = take_braced(after_numerator) {
                output.push_str("frac(");
                output.push_str(numerator.trim());
                output.push_str(", ");
                output.push_str(denominator.trim());
                output.push(')');
                rest = after_denominator;
                continue;
            }
        }
        output.push_str("\\frac");
        rest = after;
    }
    output.push_str(rest);
    output
}

fn take_braced(input: &str) -> Option<(&str, &str)> {
    let input = input.strip_prefix('{')?;
    let end = input.find('}')?;
    Some((&input[..end], &input[end + 1..]))
}

fn escape_typst_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        escape_typst_char(ch, &mut out);
    }
    out
}

fn escape_typst_char(ch: char, out: &mut String) {
    match ch {
        '\\' | '#' | '*' | '_' | '`' | '$' | '[' | ']' | '<' | '>' | '=' | '+' | '-' | '@' => {
            out.push('\\');
            out.push(ch);
        }
        _ => out.push(ch),
    }
}

fn escape_typst_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn markdown_expects_text(markdown: &str) -> bool {
    markdown.chars().any(char::is_alphanumeric)
}

fn svg_has_text_geometry(svg: &str) -> bool {
    svg.contains("class=\"typst-text\"")
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
        RenderTemplate::Magazine => b"magazine".as_slice(),
        RenderTemplate::Notebook => b"notebook".as_slice(),
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
        assert!(output.svg.contains("class=\"typst-doc\""));
        assert!(svg_has_text_geometry(&output.svg));
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
    fn markdown_converter_closes_code_fences_and_keeps_headings() {
        let (typst, diagnostics) = markdown_to_typst(
            "# 一、测试\n## 1.测试\n### 测试\n---\n```rust\nfn main() {\n  println!(\"Hello World!\");\n}",
        );

        assert!(typst.contains("= 一、测试"));
        assert!(typst.contains("== 1.测试"));
        assert!(typst.contains("=== 测试"));
        assert!(typst.contains("= 一、测试\n#v(0.92em)"));
        assert!(typst.contains("== 1.测试\n#v(0.78em)"));
        assert!(typst.contains("=== 测试\n#v(0.66em)"));
        assert!(typst.contains("#line(length: 100%"));
        assert!(typst.contains("#v(0.22em)\n```rust"));
        assert!(typst.contains("println!(\"Hello World!\");"));
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn markdown_converter_keeps_inline_emphasis_and_math_semantics() {
        let (typst, diagnostics) = markdown_to_typst(
            "Use **Markdown**, *italic*, `code`, and $ \\frac{1}{2} + \\sin{2} $.",
        );

        assert!(typst.contains("#strong[Markdown]"));
        assert!(typst.contains("#emph[italic]"));
        assert!(typst.contains("#raw(\"code\")"));
        assert!(typst.contains("$frac(1, 2) + sin(2)$"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn markdown_converter_escapes_typst_reference_syntax_in_plain_text() {
        let (typst, diagnostics) =
            markdown_to_typst("Account LaishaGavlin2608@hotmail.com should render as text.");

        assert!(typst.contains("LaishaGavlin2608\\@hotmail.com"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn markdown_converter_keeps_links_quotes_and_ordered_lists() {
        let (typst, diagnostics) = markdown_to_typst(
            "> Quote with [link](https://example.com) and ~~old~~ text.\n\n1. first\n2) second",
        );

        assert!(typst.contains("#quote(block: true)["));
        assert!(typst.contains("#link(\"https://example.com\")[link]"));
        assert!(typst.contains("#strike[old]"));
        assert!(typst.contains("+ first"));
        assert!(typst.contains("+ second"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn markdown_converter_preserves_visible_soft_breaks() {
        let (typst, diagnostics) = markdown_to_typst("first line\n*second line*");

        assert!(typst.contains("first line\\\n#emph[second line]"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn markdown_converter_wraps_top_level_paragraphs_with_spacing() {
        let (typst, diagnostics) = markdown_to_typst("first paragraph.\n\nsecond paragraph.");

        assert!(typst.contains("#block[first paragraph.]\n#v(0.82em)"));
        assert!(typst.contains("#block[second paragraph.]\n#v(0.82em)"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn markdown_converter_promotes_standalone_math() {
        let (typst, diagnostics) = markdown_to_typst("$ \\frac{1}{2} + \\sin{2} $");

        assert!(typst.contains("#block(above: 0.36em, below: 0.28em)[$frac(1, 2) + sin(2)$]"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn markdown_inline_semantics_render_with_typst() {
        let output = render_memo(RenderMemoInput {
            body: "Use **Markdown**, *italic*, and $ \\frac{1}{2} + \\sin{2} $.".to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();

        assert!(svg_has_text_geometry(&output.svg));
        assert!(output.svg.matches("<use").count() > 15);
    }

    #[test]
    fn markdown_email_address_renders_as_plain_text() {
        let output = render_memo(RenderMemoInput {
            body: "账号 LaishaGavlin2608@hotmail.com\n接码地址：https://example.com/mailbox"
                .to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();

        assert!(svg_has_text_geometry(&output.svg));
        assert!(output.svg.matches("<use").count() > 20);
    }

    #[test]
    fn markdown_rich_semantics_render_with_typst() {
        let output = render_memo(RenderMemoInput {
            body: "# Heading\n\n> Quote with [link](https://example.com).\n\n1. First\n2. Second\n\nInline `code` and ~~old~~ text."
                .to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Technical,
        })
        .unwrap();

        assert!(svg_has_text_geometry(&output.svg));
        assert!(output.svg.matches("<use").count() > 20);
    }

    #[test]
    fn markdown_sample_renders_more_than_code_block() {
        let output = render_memo(RenderMemoInput {
            body: "# 一、测试\n## 1.测试\n### 测试\n---\n```rust\nfn main() {\n  println!(\"Hello World!\");\n}".to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();

        assert!(
            output.height_pt > 100.0,
            "expected heading and code layout, got {}pt",
            output.height_pt
        );
        assert!(svg_has_text_geometry(&output.svg));
        assert!(
            output.svg.matches("<use").count() > 20,
            "expected Typst to emit real glyph references"
        );
        assert!(!output.diagnostics.is_empty());
    }

    #[test]
    fn markdown_smoke_renders_svg() {
        let output = render_memo(RenderMemoInput {
            body: "# Hello\n\nUse **Markdown**.".to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();
        assert!(output.svg.contains("<svg"));
        assert!(svg_has_text_geometry(&output.svg));
    }

    #[test]
    fn embedded_fonts_render_chinese_and_code() {
        let output = render_memo(RenderMemoInput {
            body: "# 备忘录\n\n中文 English mixed text.\n\n```rust\nfn main() {}\n```".to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();

        assert!(output.svg.contains("<defs"));
        assert!(output.svg.matches("<use").count() > 20);
        assert!(svg_has_text_geometry(&output.svg));
    }

    #[test]
    fn typst_preview_page_is_transparent() {
        let output = render_memo(RenderMemoInput {
            body: "Transparent preview surface".to_string(),
            format: RenderFormat::Markdown,
            template: RenderTemplate::Literary,
        })
        .unwrap();

        assert!(
            !output.svg.contains("fill=\"#ffffff\""),
            "preview SVG should not paint its own white page"
        );
    }

    #[test]
    fn every_preview_template_renders_text_and_code() {
        for template in [
            RenderTemplate::Literary,
            RenderTemplate::Compact,
            RenderTemplate::Technical,
            RenderTemplate::Magazine,
            RenderTemplate::Notebook,
        ] {
            let output = render_memo(RenderMemoInput {
                body: "# Preview\n\n中文 typography sample.\n\n```rust\nfn main() {}\n```"
                    .to_string(),
                format: RenderFormat::Markdown,
                template,
            })
            .unwrap();

            assert!(svg_has_text_geometry(&output.svg));
            assert!(output.svg.matches("<use").count() > 20);
        }
    }
}
