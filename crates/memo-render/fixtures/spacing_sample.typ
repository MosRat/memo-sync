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

= 一、测试
#v(0.92em)
#block[this is a par.]
#v(0.82em)

== 1.测试
#v(0.78em)
#block[this is a par.]
#v(0.82em)

=== 测试
#v(0.66em)
#block[this is a par.]
#v(0.82em)

#line(length: 100%, stroke: 0.55pt + rgb("#d9cbb5"))
#v(0.74em)

```rust
fn main() {
  println!("Hello World!");
}
```
#v(0.78em)

#block[this is a par.]
#v(0.82em)
#block[another par.]
#v(0.82em)
