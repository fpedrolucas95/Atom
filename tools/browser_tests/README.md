# browser_tests

Host-side regression tests for the Atom browser's HTML5 + CSS engine
(`userspace/apps/browser`). The engine modules are `no_std + alloc` and only
depend on `libgui::color`/`libimage` types, so they are included here via
`#[path]` against the stub crates in `stubs/` and tested with the host
toolchain.

```sh
cd tools/browser_tests
cargo test
```

The local `.cargo/config.toml` overrides the repository-wide UEFI build target
with `x86_64-unknown-linux-gnu`; on a non-Linux host, pass your own triple:
`cargo test --target <host-triple>`.

Coverage: tokenizer states (comments, CDATA, RCDATA/RAWTEXT, script-data
escaping, character references incl. legacy semicolon-less forms), tree
construction (implied end tags, scope-aware closing, formatting reconstruction
and misnesting recovery, depth bounding, foster parenting), CSS (selectors,
specificity, `!important`, combinators, structural pseudo-classes, `@media`,
color formats, external `<link>` sheets, `font-size`), style computation
(inheritance, presentational attributes), the flattener (lists, tables, forms,
whitespace, `pre`, visibility), and the JavaScript engine (language core —
closures, prototypes, exceptions, arrows, ASI, Math/JSON —, DOM bindings —
`getElementById`, `querySelector`, `document.write`, `innerHTML`,
`createElement`, `style` —, the event model — `addEventListener`/`onclick`
property and attribute handlers, bubbling, `preventDefault`/`stopPropagation`,
`DOMContentLoaded`/`load`, click-zone hit regions, re-flattening after
dispatch —, and the runaway-script bounds: step budget and call-depth aborts
that keep the page rendering).
