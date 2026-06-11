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
and misnesting recovery, depth bounding), CSS (selectors, specificity,
`!important`, combinators, structural pseudo-classes, `@media`, color
formats), style computation (inheritance, presentational attributes), and the
flattener (lists, tables, forms, whitespace, `pre`, visibility).
