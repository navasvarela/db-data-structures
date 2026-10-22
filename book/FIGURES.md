# Figures

Figures are hand-written SVG files, kept in a `figures/` folder next to the pages that use them
and named after the page (`bloom-filter-insert.svg`). A page pulls one in with mdBook's
`{{#include}}`, inside a `<figure>`:

```html
<figure class="diagram">
{{#include figures/bloom-filter-insert.svg}}
<figcaption>Inserting two keys with k = 3.</figcaption>
</figure>
```

The SVG ends up inline in the page, rather than loaded through `<img>`, so it can use the classes
in [`figures.css`](figures.css), which follow the reader's light or dark theme. Never hard-code a
colour in a figure; use the classes listed at the top of that file.

## Rules

- **No blank lines** in the SVG file or between the `<figure>` tags. Markdown ends an HTML block
  at a blank line, and the rest of the SVG would then be rendered as text or code.
- The root element is `<svg class="diagram" viewBox="0 0 W H" ...>` with no `width` or `height`,
  so it scales down on narrow screens. Keep `W` at 640 or less, the width of the text column.
- Give the `<svg>` a `<title>` that describes the figure for screen readers, referenced with
  `role="img"` and `aria-labelledby`.
- Prefix every `id` (arrowhead markers, the title) with the file name. Several figures can share
  a page, and duplicate ids would make one figure use another's markers.
- Text is at least 12px (`.t-small`), and labels are short; `figcaption` carries the explanation.
- Captions are plain HTML, not Markdown: use `<code>` rather than backticks.
