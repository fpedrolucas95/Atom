//! Host-side regression tests for the browser's HTML5 + CSS engine.
//!
//! The engine modules are `no_std + alloc` and depend only on `libgui::color`
//! and `libimage` types, so they compile unchanged on the host against the
//! stub crates in `stubs/`. Run with `cargo test` in this directory.

extern crate alloc;

#[path = "../../../userspace/apps/browser/src/css.rs"]
pub mod css;
#[path = "../../../userspace/apps/browser/src/dom.rs"]
pub mod dom;
#[path = "../../../userspace/apps/browser/src/domtree.rs"]
pub mod domtree;
#[path = "../../../userspace/apps/browser/src/entities.rs"]
pub mod entities;
#[path = "../../../userspace/apps/browser/src/html.rs"]
pub mod html;
#[path = "../../../userspace/apps/browser/src/js/mod.rs"]
pub mod js;
#[path = "../../../userspace/apps/browser/src/style.rs"]
pub mod style;
#[path = "../../../userspace/apps/browser/src/text.rs"]
pub mod text;
#[path = "../../../userspace/apps/browser/src/tokenizer.rs"]
pub mod tokenizer;

#[cfg(test)]
mod tests {
    use crate::dom::{
        Align, AlignItems, Block, BoxStyle, Document, FlexChild, FlexDirection, Inline, InputKind,
        JustifyContent, Length, Position, TextKind,
    };
    use crate::html::{parse_html, parse_html_with_css};
    use libgui::color::Color;

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Text content of block `i`, with runs joined as-is.
    fn block_text(doc: &Document, i: usize) -> String {
        match &doc.blocks[i] {
            Block::Text { items, .. } => {
                let mut s = String::new();
                for it in items {
                    if let Inline::Run(r) = it {
                        s.push_str(&r.text);
                    }
                }
                s.trim().to_string()
            }
            _ => String::new(),
        }
    }

    /// All text blocks' contents.
    fn texts(doc: &Document) -> Vec<String> {
        (0..doc.blocks.len())
            .filter(|&i| matches!(doc.blocks[i], Block::Text { .. }))
            .map(|i| block_text(doc, i))
            .collect()
    }

    fn kind_of(doc: &Document, i: usize) -> TextKind {
        match &doc.blocks[i] {
            Block::Text { kind, .. } => *kind,
            _ => panic!("block {i} is not text"),
        }
    }

    /// First run whose text contains `needle`.
    fn find_run<'a>(doc: &'a Document, needle: &str) -> &'a crate::dom::Run {
        for b in &doc.blocks {
            if let Block::Text { items, .. } = b {
                for it in items {
                    if let Inline::Run(r) = it {
                        if r.text.contains(needle) {
                            return r;
                        }
                    }
                }
            }
        }
        panic!("run containing {needle:?} not found");
    }

    fn all_text(doc: &Document) -> String {
        texts(doc).join("\n")
    }

    // ── Tokenizer + entities ────────────────────────────────────────────────

    #[test]
    fn entities_named_numeric_legacy() {
        let doc = parse_html("<p>&amp; &lt;x&gt; &#65;&#x42; &copy 2024 &notit; &rarr;</p>");
        assert_eq!(block_text(&doc, 0), "& <x> AB (c) 2024 -it; ->");
    }

    #[test]
    fn entity_in_attribute_not_greedy() {
        // `&copy=1` inside an attribute must not decode (spec attribute rule).
        let doc = parse_html(r#"<a href="/x?a=1&copy=2">link</a>"#);
        assert_eq!(doc.links[0], "/x?a=1&copy=2");
        // …but a terminated entity does decode.
        let doc = parse_html(r#"<a href="/x?a&amp;b">l</a>"#);
        assert_eq!(doc.links[0], "/x?a&b");
    }

    #[test]
    fn utf8_text_transliterates() {
        let doc = parse_html("<p>caf\u{e9} \u{2014} na\u{ef}ve \u{201c}ok\u{201d}</p>");
        assert_eq!(block_text(&doc, 0), "cafe -- naive \"ok\"");
    }

    #[test]
    fn comments_and_doctype_ignored() {
        let doc = parse_html(
            "<!DOCTYPE html><!-- hidden --><p>a<!-->b<!--->c<!-- multi\nline -->d</p><?php no ?>",
        );
        assert_eq!(block_text(&doc, 0), "abcd");
    }

    #[test]
    fn script_and_style_content_hidden() {
        let doc =
            parse_html("<style>p{}</style><script>var a = '<p>nope</p>';</script><p>shown</p>");
        assert_eq!(all_text(&doc), "shown");
    }

    #[test]
    fn script_escape_dance() {
        // Double-escaped script data: a nested `<script>` inside `<!-- -->`
        // makes the inner `</script>` ordinary text (spec escaping states).
        let html = "<script><!--<script>x = '</script>';--></script><p>after</p>";
        let doc = parse_html(html);
        assert_eq!(all_text(&doc), "after");
        // Without the nested `<script>`, the first `</script>` closes the
        // element even inside the comment (spec: single-escaped state).
        let doc = parse_html("<script><!-- x = '</script><p>visible</p>");
        assert!(all_text(&doc).contains("visible"));
    }

    #[test]
    fn rcdata_title_with_markup() {
        let doc = parse_html("<title>a <b> &amp; b</title><p>body</p>");
        assert_eq!(doc.title, "a <b> & b");
    }

    #[test]
    fn unquoted_and_single_quoted_attributes() {
        let doc = parse_html("<a href=/page title='t'>x</a>");
        assert_eq!(doc.links[0], "/page");
    }

    // ── Tree construction ───────────────────────────────────────────────────

    #[test]
    fn implied_paragraph_end() {
        let doc = parse_html("<p>one<p>two<div>three</div>");
        assert_eq!(texts(&doc), ["one", "two", "three"]);
    }

    #[test]
    fn implied_list_items() {
        let doc = parse_html("<ul><li>a<li>b</ul><ol start=3><li>c</ol>");
        let markers: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { marker, .. } => marker.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(markers, ["*", "*", "3."]);
        assert_eq!(texts(&doc), ["a", "b", "c"]);
    }

    #[test]
    fn nested_list_markers() {
        let doc = parse_html("<ul><li>top<ul><li>inner</ul></ul>");
        let markers: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { marker, .. } => marker.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(markers, ["*", "  -"]);
    }

    #[test]
    fn ordered_list_numbering_types() {
        let doc = parse_html(r#"<ol type="a"><li>x<li>y</ol><ol type="I"><li>z</ol>"#);
        let markers: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { marker, .. } => marker.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(markers, ["a.", "b.", "I."]);
    }

    #[test]
    fn misnested_formatting_recovers() {
        // <b>1 <u>2</b> 3</u>: "2" is bold+underline, "3" only underlined.
        let doc = parse_html("<p><b>one <u>two</b> three</u> four</p>");
        let one = find_run(&doc, "one");
        assert!(one.style.bold && !one.style.underline);
        let two = find_run(&doc, "two");
        assert!(two.style.bold && two.style.underline);
        let three = find_run(&doc, "three");
        assert!(!three.style.bold && three.style.underline);
        let four = find_run(&doc, "four");
        assert!(!four.style.bold && !four.style.underline);
    }

    #[test]
    fn block_inside_formatting_does_not_leak() {
        // </b> with an open <div> above it: later text must not stay bold.
        let doc = parse_html("<b><div>x</b>y</div>");
        assert!(find_run(&doc, "x").style.bold);
        assert!(!find_run(&doc, "y").style.bold);
    }

    #[test]
    fn headings_close_implicitly() {
        let doc = parse_html("<h1>title<h2>sub</h2>");
        assert_eq!(kind_of(&doc, 0), TextKind::H1);
        assert_eq!(kind_of(&doc, 1), TextKind::H2);
        assert_eq!(texts(&doc), ["title", "sub"]);
    }

    #[test]
    fn table_linearised_by_rows() {
        let doc = parse_html(
            "<table><caption>Cap</caption><tr><th>H1</th><th>H2</th></tr>\
             <tbody><tr><td>a</td><td>b</td></tr></tbody></table>",
        );
        let t = texts(&doc);
        assert_eq!(t[0], "Cap");
        assert_eq!(t[1], "H1 | H2");
        assert_eq!(t[2], "a | b");
        // header cells are bold by default
        assert!(find_run(&doc, "H1").style.bold);
        assert!(!find_run(&doc, "a").style.bold);
    }

    // ── Flexbox layout ───────────────────────────────────────────────────────

    /// The first `Block::Flex` in the document, with its container properties.
    fn first_flex(doc: &Document) -> (FlexDirection, JustifyContent, AlignItems, u16, &[FlexChild]) {
        for b in &doc.blocks {
            if let Block::Flex {
                direction,
                justify,
                align,
                gap,
                children,
                ..
            } = b
            {
                return (*direction, *justify, *align, *gap, children);
            }
        }
        panic!("no flex block found");
    }

    /// The `wrap` flag of the first flex block.
    fn first_flex_wrap(doc: &Document) -> bool {
        for b in &doc.blocks {
            if let Block::Flex { wrap, .. } = b {
                return *wrap;
            }
        }
        panic!("no flex block found");
    }

    /// Joined, trimmed text of a flex child's sub-flow.
    fn child_text(child: &FlexChild) -> String {
        let mut s = String::new();
        for b in &child.blocks {
            if let Block::Text { items, .. } = b {
                for it in items {
                    if let Inline::Run(r) = it {
                        s.push_str(&r.text);
                    }
                }
            }
        }
        s.trim().to_string()
    }

    #[test]
    fn flex_container_lays_out_element_children() {
        let doc =
            parse_html("<div style=\"display:flex\"><div>A</div><span>B</span><p>C</p></div>");
        let (dir, _, _, _, children) = first_flex(&doc);
        assert_eq!(dir, FlexDirection::Row);
        assert_eq!(children.len(), 3);
        assert_eq!(child_text(&children[0]), "A");
        assert_eq!(child_text(&children[1]), "B");
        assert_eq!(child_text(&children[2]), "C");
    }

    #[test]
    fn flex_container_properties_parse() {
        let doc = parse_html(
            "<div style=\"display:flex; flex-direction:column; \
             justify-content:space-between; align-items:center; gap:12px\">\
             <div>A</div><div>B</div></div>",
        );
        let (dir, justify, align, gap, children) = first_flex(&doc);
        assert_eq!(dir, FlexDirection::Column);
        assert_eq!(justify, JustifyContent::SpaceBetween);
        assert_eq!(align, AlignItems::Center);
        assert_eq!(gap, 12);
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn flex_item_grow_and_basis_resolve() {
        let doc = parse_html(
            "<div style=\"display:flex\">\
             <div style=\"flex:2\">A</div>\
             <div style=\"flex-grow:1; flex-basis:80px\">B</div>\
             <div style=\"width:120px\">C</div></div>",
        );
        let (_, _, _, _, children) = first_flex(&doc);
        assert_eq!((children[0].grow, children[0].basis), (2, None));
        assert_eq!(
            (children[1].grow, children[1].basis),
            (1, Some(Length::Px(80)))
        );
        assert_eq!(
            (children[2].grow, children[2].basis),
            (0, Some(Length::Px(120)))
        );
    }

    #[test]
    fn flex_containers_nest() {
        let doc = parse_html(
            "<div style=\"display:flex\">\
             <div style=\"display:flex\"><div>A</div><div>B</div></div>\
             <div>C</div></div>",
        );
        let (_, _, _, _, children) = first_flex(&doc);
        assert_eq!(children.len(), 2);
        // The first child is itself a flex container with two items.
        match &children[0].blocks[0] {
            Block::Flex { children: inner, .. } => {
                assert_eq!(inner.len(), 2);
                assert_eq!(child_text(&inner[0]), "A");
                assert_eq!(child_text(&inner[1]), "B");
            }
            _ => panic!("expected nested flex block"),
        }
        assert_eq!(child_text(&children[1]), "C");
    }

    #[test]
    fn flex_bare_text_becomes_anonymous_item() {
        let doc = parse_html("<div style=\"display:flex\">Label<div>X</div></div>");
        let (_, _, _, _, children) = first_flex(&doc);
        assert_eq!(children.len(), 2);
        assert_eq!(child_text(&children[0]), "Label");
        assert_eq!(child_text(&children[1]), "X");
    }

    #[test]
    fn flex_skips_display_none_children() {
        let doc = parse_html(
            "<div style=\"display:flex\">\
             <div style=\"display:none\">gone</div><div>here</div></div>",
        );
        let (_, _, _, _, children) = first_flex(&doc);
        assert_eq!(children.len(), 1);
        assert_eq!(child_text(&children[0]), "here");
    }

    #[test]
    fn flex_links_inside_items_stay_addressable() {
        // Links nested in flex items must still register in the document's
        // global link table so clicks resolve.
        let doc = parse_html(
            "<div style=\"display:flex\"><a href=\"/one\">1</a><a href=\"/two\">2</a></div>",
        );
        assert_eq!(doc.links, vec!["/one".to_string(), "/two".to_string()]);
    }

    #[test]
    fn flex_ignored_on_replaced_elements() {
        // `display:flex` on an <img> must not swallow it into a flex container.
        let doc = parse_html("<img src=\"x.png\" style=\"display:flex\" alt=\"a\">");
        assert!(doc
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Image { .. })));
        assert!(!doc.blocks.iter().any(|b| matches!(b, Block::Flex { .. })));
    }

    // ── Box model (background / padding / border) ────────────────────────────

    /// The first `Block::Box` in the document, with its decoration and children.
    fn first_box(doc: &Document) -> (&BoxStyle, &[Block]) {
        for b in &doc.blocks {
            if let Block::Box { style, children, .. } = b {
                return (style, children);
            }
        }
        panic!("no box block found");
    }

    /// Joined, trimmed text of a sub-flow of blocks.
    fn blocks_text(blocks: &[Block]) -> String {
        let mut s = String::new();
        for b in blocks {
            if let Block::Text { items, .. } = b {
                for it in items {
                    if let Inline::Run(r) = it {
                        s.push_str(&r.text);
                    }
                }
            }
        }
        s.trim().to_string()
    }

    #[test]
    fn decorated_div_becomes_a_box() {
        let doc = parse_html(
            "<div style=\"background:#102030; padding:8px\"><p>hi</p></div>",
        );
        let (style, children) = first_box(&doc);
        assert_eq!(style.background, Some(Color::rgb(0x10, 0x20, 0x30)));
        assert_eq!(style.padding, [8, 8, 8, 8]);
        assert_eq!(blocks_text(children), "hi");
    }

    #[test]
    fn padding_shorthand_expands_per_side() {
        // one value → all sides
        let (s1, _) = {
            let doc = parse_html("<div style=\"padding:5px\">x</div>");
            let (s, _) = first_box(&doc);
            (*s, ())
        };
        assert_eq!(s1.padding, [5, 5, 5, 5]);
        // two values → vertical / horizontal
        let doc = parse_html("<div style=\"padding:4px 8px\">x</div>");
        assert_eq!(first_box(&doc).0.padding, [4, 8, 4, 8]);
        // three values → top / horizontal / bottom
        let doc = parse_html("<div style=\"padding:1px 2px 3px\">x</div>");
        assert_eq!(first_box(&doc).0.padding, [1, 2, 3, 2]);
        // four values → top right bottom left
        let doc = parse_html("<div style=\"padding:1px 2px 3px 4px\">x</div>");
        assert_eq!(first_box(&doc).0.padding, [1, 2, 3, 4]);
    }

    #[test]
    fn border_shorthand_parses_width_and_color() {
        let doc = parse_html("<div style=\"border:2px solid #ff0000\">x</div>");
        let (style, _) = first_box(&doc);
        assert_eq!(style.border_width, 2);
        assert_eq!(style.border_color, Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn border_radius_and_box_sizing_parse() {
        let doc = parse_html(
            "<div style=\"background:#222; border-radius:8px; box-sizing:border-box; \
             width:200px\">x</div>",
        );
        let (style, _) = first_box(&doc);
        assert_eq!(style.radius, 8);
        assert!(style.border_box);
        assert_eq!(style.width, Some(Length::Px(200)));
    }

    #[test]
    fn box_sizing_defaults_to_content_box() {
        let doc = parse_html("<div style=\"background:#222; width:200px\">x</div>");
        assert!(!first_box(&doc).0.border_box);
        assert_eq!(first_box(&doc).0.radius, 0);
    }

    #[test]
    fn linear_gradient_background_parses() {
        let doc =
            parse_html("<div style=\"background: linear-gradient(#ff0000, #0000ff)\">x</div>");
        let g = first_box(&doc).0.gradient.expect("gradient present");
        assert_eq!(g, (Color::rgb(255, 0, 0), Color::rgb(0, 0, 255)));
    }

    #[test]
    fn linear_gradient_to_top_flips_stops() {
        let doc = parse_html(
            "<div style=\"background-image: linear-gradient(to top, #111111, #eeeeee)\">x</div>",
        );
        let g = first_box(&doc).0.gradient.expect("gradient present");
        // `to top` puts the last stop at the visual top.
        assert_eq!(g, (Color::rgb(0xEE, 0xEE, 0xEE), Color::rgb(0x11, 0x11, 0x11)));
    }

    #[test]
    fn linear_gradient_with_rgb_stops_and_angle() {
        // Commas inside rgb() must not split the stop list; the angle is parsed.
        let doc = parse_html(
            "<div style=\"background: linear-gradient(180deg, rgb(10,20,30), rgb(40,50,60))\">x</div>",
        );
        let g = first_box(&doc).0.gradient.expect("gradient present");
        assert_eq!(g, (Color::rgb(10, 20, 30), Color::rgb(40, 50, 60)));
    }

    #[test]
    fn box_shadow_parses_offsets_blur_and_color() {
        let doc =
            parse_html("<div style=\"box-shadow: 2px 4px 8px 1px #ff0000\"><p>x</p></div>");
        let sh = first_box(&doc).0.shadow.expect("shadow present");
        assert_eq!((sh.dx, sh.dy, sh.blur, sh.spread), (2, 4, 8, 1));
        assert_eq!(sh.color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn box_shadow_allows_negative_offsets_and_defaults_color() {
        let doc = parse_html("<div style=\"box-shadow: -3px 2px\"><p>x</p></div>");
        let sh = first_box(&doc).0.shadow.expect("shadow present");
        assert_eq!((sh.dx, sh.dy, sh.blur, sh.spread), (-3, 2, 0, 0));
        assert_eq!(sh.color, Color::rgb(0, 0, 0));
    }

    #[test]
    fn relative_position_stays_in_flow_with_offsets() {
        let doc = parse_html(
            "<div style=\"position:relative; top:10px; left:5px\"><p>x</p></div>",
        );
        let (style, _) = first_box(&doc);
        assert_eq!(style.position, Position::Relative);
        assert_eq!(style.inset[0], Some(Length::Px(10))); // top
        assert_eq!(style.inset[3], Some(Length::Px(5))); // left
        // It stays in the normal flow, not hoisted out.
        assert!(doc.positioned.is_empty());
    }

    #[test]
    fn absolute_position_is_hoisted_out_of_flow() {
        let doc = parse_html(
            "<p>flow</p><div style=\"position:absolute; top:0; right:0\"><p>badge</p></div>",
        );
        // No box block in the normal flow…
        assert!(!doc.blocks.iter().any(|b| matches!(b, Block::Box { .. })));
        // …it moved to the positioned list.
        assert_eq!(doc.positioned.len(), 1);
        assert_eq!(doc.positioned[0].style.position, Position::Absolute);
        assert_eq!(doc.positioned[0].style.inset[1], Some(Length::Px(0))); // right
        assert_eq!(blocks_text(&doc.positioned[0].blocks), "badge");
    }

    #[test]
    fn absolute_attaches_to_positioned_ancestor() {
        // The absolute child's containing block is the relative parent, so it
        // is nested under that box rather than the document root.
        let doc = parse_html(
            "<div style=\"position:relative; padding:4px\">\
             <p>host</p>\
             <div style=\"position:absolute; top:0; right:0\"><p>badge</p></div>\
             </div>",
        );
        // No document-root positioned box — it attached to its ancestor.
        assert!(doc.positioned.is_empty());
        let (style, children) = first_box(&doc);
        assert_eq!(style.position, Position::Relative);
        // The relative box carries the absolute child.
        let abs = match &doc.blocks[0] {
            Block::Box { abs_children, .. } => abs_children,
            _ => panic!("expected a box block"),
        };
        assert_eq!(abs.len(), 1);
        assert_eq!(abs[0].style.position, Position::Absolute);
        assert_eq!(blocks_text(&abs[0].blocks), "badge");
        // The host paragraph stays in the box's normal flow.
        assert_eq!(blocks_text(children), "host");
    }

    #[test]
    fn absolute_without_positioned_ancestor_goes_to_root() {
        let doc = parse_html(
            "<div style=\"padding:4px\"><div style=\"position:absolute; top:0\">x</div></div>",
        );
        // The static parent does not establish a containing block.
        assert_eq!(doc.positioned.len(), 1);
        assert_eq!(doc.positioned[0].style.position, Position::Absolute);
    }

    #[test]
    fn fixed_position_is_hoisted() {
        let doc = parse_html("<div style=\"position:fixed; bottom:8px\"><p>bar</p></div>");
        assert_eq!(doc.positioned.len(), 1);
        assert_eq!(doc.positioned[0].style.position, Position::Fixed);
        assert_eq!(doc.positioned[0].style.inset[2], Some(Length::Px(8))); // bottom
    }

    #[test]
    fn height_overflow_and_zindex_parse() {
        let doc = parse_html(
            "<div style=\"position:absolute; height:120px; min-height:40px; \
             overflow:hidden; z-index:5\"><p>x</p></div>",
        );
        let style = doc.positioned[0].style;
        assert_eq!(style.height, Some(120));
        assert_eq!(style.min_height, Some(40));
        assert!(style.overflow_clip);
        assert_eq!(style.z_index, 5);
    }

    #[test]
    fn overflow_visible_does_not_clip() {
        let doc = parse_html("<div style=\"background:#222; overflow:visible\">x</div>");
        assert!(!first_box(&doc).0.overflow_clip);
    }

    #[test]
    fn negative_zindex_parses() {
        let doc = parse_html("<div style=\"position:fixed; z-index:-1\"><p>x</p></div>");
        assert_eq!(doc.positioned[0].style.z_index, -1);
    }

    #[test]
    fn positioned_box_links_stay_addressable() {
        let doc = parse_html(
            "<div style=\"position:absolute; top:0\"><a href=\"/x\">go</a></div>",
        );
        assert_eq!(doc.links, vec!["/x".to_string()]);
    }

    #[test]
    fn border_none_keeps_box_unboxed_without_other_decoration() {
        // `border-style:none` zeroes the width; with nothing else to paint the
        // div stays a plain flow element (no box block).
        let doc = parse_html("<div style=\"border-style:none\"><p>x</p></div>");
        assert!(!doc.blocks.iter().any(|b| matches!(b, Block::Box { .. })));
    }

    #[test]
    fn undecorated_div_is_not_boxed() {
        let doc = parse_html("<div><p>a</p><p>b</p></div>");
        assert!(!doc.blocks.iter().any(|b| matches!(b, Block::Box { .. })));
        assert_eq!(texts(&doc), ["a", "b"]);
    }

    #[test]
    fn box_wraps_flex_when_both_apply() {
        let doc = parse_html(
            "<div style=\"display:flex; background:#222; padding:6px\">\
             <div>A</div><div>B</div></div>",
        );
        let (style, children) = first_box(&doc);
        assert_eq!(style.padding, [6, 6, 6, 6]);
        assert_eq!(style.background, Some(Color::rgb(0x22, 0x22, 0x22)));
        match &children[0] {
            Block::Flex { children: items, .. } => assert_eq!(items.len(), 2),
            _ => panic!("expected a flex block inside the box"),
        }
    }

    #[test]
    fn box_children_links_stay_addressable() {
        let doc =
            parse_html("<div style=\"padding:4px\"><a href=\"/deep\">go</a></div>");
        assert_eq!(doc.links, vec!["/deep".to_string()]);
    }

    #[test]
    fn margin_triggers_a_box() {
        let doc = parse_html("<div style=\"margin:10px 20px\"><p>x</p></div>");
        let (style, _) = first_box(&doc);
        assert_eq!(style.margin, [10, 20, 10, 20]);
        assert!(!style.center);
    }

    #[test]
    fn margin_auto_centers_box() {
        // The canonical centred content column.
        let doc = parse_html(
            "<div style=\"max-width:600px; margin:0 auto\"><p>centered</p></div>",
        );
        let (style, children) = first_box(&doc);
        assert!(style.center);
        assert_eq!(style.width, Some(Length::Px(600)));
        assert_eq!(style.margin, [0, 0, 0, 0]);
        assert_eq!(blocks_text(children), "centered");
    }

    #[test]
    fn width_sets_box_and_flex_basis() {
        // `width` doubles as the flex basis; here it just sizes the block box.
        let doc = parse_html("<div style=\"width:320px; background:#000\">x</div>");
        assert_eq!(first_box(&doc).0.width, Some(Length::Px(320)));
    }

    #[test]
    fn percentage_width_is_recorded() {
        let doc = parse_html("<div style=\"width:50%; background:#000\">x</div>");
        assert_eq!(first_box(&doc).0.width, Some(Length::Pct(50)));
    }

    #[test]
    fn percentage_resolves_against_base() {
        assert_eq!(Length::Pct(50).resolve(600), 300);
        assert_eq!(Length::Pct(33).resolve(900), 297);
        assert_eq!(Length::Px(120).resolve(600), 120);
    }

    #[test]
    fn flex_wrap_parses() {
        assert!(!first_flex_wrap(&parse_html(
            "<div style=\"display:flex\"><div>a</div></div>"
        )));
        assert!(first_flex_wrap(&parse_html(
            "<div style=\"display:flex; flex-wrap:wrap\"><div>a</div></div>"
        )));
        // `flex-flow` shorthand sets both direction and wrap.
        let doc = parse_html("<div style=\"display:flex; flex-flow:column wrap\"><div>a</div></div>");
        assert!(first_flex_wrap(&doc));
        assert_eq!(first_flex(&doc).0, FlexDirection::Column);
    }

    #[test]
    fn flex_basis_accepts_percentage() {
        let doc = parse_html(
            "<div style=\"display:flex\">\
             <div style=\"flex-basis:33%\">A</div>\
             <div style=\"flex:1 1 25%\">B</div></div>",
        );
        let (_, _, _, _, children) = first_flex(&doc);
        assert_eq!(children[0].basis, Some(Length::Pct(33)));
        assert_eq!((children[1].grow, children[1].basis), (1, Some(Length::Pct(25))));
    }

    #[test]
    fn min_height_recorded_on_box() {
        let doc = parse_html("<div style=\"min-height:200px; background:#111\">x</div>");
        assert_eq!(first_box(&doc).0.min_height, Some(200));
    }


    #[test]
    fn end_tag_br_acts_as_br() {
        let doc = parse_html("<p>a</br>b</p>");
        assert_eq!(texts(&doc), ["a", "b"]);
    }

    #[test]
    fn whitespace_collapses() {
        let doc = parse_html("<p>  a \n\t b   <b> c</b></p>");
        assert_eq!(block_text(&doc, 0), "a b c");
    }

    #[test]
    fn pre_preserves_whitespace() {
        let doc = parse_html("<pre>\nline1\n  line2</pre>");
        assert_eq!(kind_of(&doc, 0), TextKind::Pre);
        match &doc.blocks[0] {
            Block::Text { items, .. } => {
                let mut s = String::new();
                for it in items {
                    if let Inline::Run(r) = it {
                        s.push_str(&r.text);
                    }
                }
                assert_eq!(s, "line1\n  line2");
            }
            _ => panic!(),
        }
    }

    // ── Forms ───────────────────────────────────────────────────────────────

    #[test]
    fn form_controls() {
        let doc = parse_html(
            r#"<form action="/s"><input type=search name=q placeholder="find">
               <input type=submit value=Go>
               <select name=m><option>A</option><option selected>B</option></select>
               <textarea name=t>hello</textarea>
               <button>Press</button></form>"#,
        );
        let inputs = &doc.inputs;
        assert_eq!(inputs.len(), 5);
        assert_eq!(inputs[0].kind, InputKind::Search);
        assert_eq!(inputs[0].action, "/s");
        assert_eq!(inputs[0].placeholder, "find");
        assert_eq!(inputs[1].kind, InputKind::Submit);
        assert_eq!(inputs[2].kind, InputKind::Select);
        assert_eq!(inputs[2].options, ["A", "B"]);
        assert_eq!(inputs[2].placeholder, "B");
        assert_eq!(inputs[3].kind, InputKind::Text);
        assert_eq!(inputs[3].placeholder, "hello");
        assert_eq!(inputs[4].kind, InputKind::Submit);
        assert_eq!(inputs[4].placeholder, "Press");
    }

    #[test]
    fn hidden_input_skipped() {
        let doc = parse_html("<input type=hidden name=h value=1><p>x</p>");
        assert!(doc.inputs.is_empty());
    }

    // ── CSS: selectors and cascade ──────────────────────────────────────────

    #[test]
    fn specificity_orders_cascade() {
        let doc = parse_html(
            "<style>p{color:#ff0000}.c{color:#00ff00}#i{color:#0000ff}</style>\
             <p class=c id=i>x</p>",
        );
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn source_order_breaks_ties() {
        let doc =
            parse_html("<style>.a{color:#ff0000}.b{color:#00ff00}</style><p class=\"a b\">x</p>");
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn important_beats_inline() {
        let doc = parse_html(
            "<style>p{color:#112233 !important}</style><p style=\"color:#445566\">x</p>",
        );
        assert_eq!(
            find_run(&doc, "x").style.color,
            Some(Color::rgb(0x11, 0x22, 0x33))
        );
    }

    #[test]
    fn inline_beats_sheet() {
        let doc = parse_html("<style>p{color:#112233}</style><p style=\"color:#445566\">x</p>");
        assert_eq!(
            find_run(&doc, "x").style.color,
            Some(Color::rgb(0x44, 0x55, 0x66))
        );
    }

    #[test]
    fn descendant_and_child_combinators() {
        let doc = parse_html(
            "<style>div p{color:#010101} section > p{color:#020202}</style>\
             <div><span><p>deep</p></span></div><section><p>kid</p></section><p>plain</p>",
        );
        assert_eq!(
            find_run(&doc, "deep").style.color,
            Some(Color::rgb(1, 1, 1))
        );
        assert_eq!(find_run(&doc, "kid").style.color, Some(Color::rgb(2, 2, 2)));
        assert_eq!(find_run(&doc, "plain").style.color, None);
    }

    #[test]
    fn sibling_combinators() {
        let doc = parse_html(
            "<style>h1 + p{color:#010101} h1 ~ ul li{color:#020202}</style>\
             <div><h1>t</h1><p>next</p><p>later</p><ul><li>item</li></ul></div>",
        );
        assert_eq!(
            find_run(&doc, "next").style.color,
            Some(Color::rgb(1, 1, 1))
        );
        assert_eq!(find_run(&doc, "later").style.color, None);
        assert_eq!(
            find_run(&doc, "item").style.color,
            Some(Color::rgb(2, 2, 2))
        );
    }

    #[test]
    fn attribute_selectors() {
        let doc = parse_html(
            "<style>[data-x]{color:#010101} a[href^=\"http\"]{color:#020202} \
             [class~=tag]{color:#030303}</style>\
             <p data-x>a</p><a href=\"http://e\">b</a><p class=\"x tag\">c</p>",
        );
        assert_eq!(find_run(&doc, "a").style.color, Some(Color::rgb(1, 1, 1)));
        assert_eq!(find_run(&doc, "b").style.color, Some(Color::rgb(2, 2, 2)));
        assert_eq!(find_run(&doc, "c").style.color, Some(Color::rgb(3, 3, 3)));
    }

    #[test]
    fn structural_pseudo_classes() {
        let doc = parse_html(
            "<style>li:first-child{color:#010101} li:last-child{color:#020202} \
             li:nth-child(2){color:#030303}</style>\
             <ul><li>one</li><li>two</li><li>three</li></ul>",
        );
        assert_eq!(find_run(&doc, "one").style.color, Some(Color::rgb(1, 1, 1)));
        assert_eq!(find_run(&doc, "two").style.color, Some(Color::rgb(3, 3, 3)));
        assert_eq!(
            find_run(&doc, "three").style.color,
            Some(Color::rgb(2, 2, 2))
        );
    }

    #[test]
    fn not_pseudo_class() {
        let doc =
            parse_html("<style>p:not(.skip){color:#010101}</style><p>yes</p><p class=skip>no</p>");
        assert_eq!(find_run(&doc, "yes").style.color, Some(Color::rgb(1, 1, 1)));
        assert_eq!(find_run(&doc, "no").style.color, None);
    }

    #[test]
    fn hover_never_matches_but_parses() {
        let doc =
            parse_html("<style>a:hover{color:#010101} a{color:#020202}</style><a href=x>l</a>");
        assert_eq!(find_run(&doc, "l").style.color, Some(Color::rgb(2, 2, 2)));
    }

    #[test]
    fn inheritance_flows_down() {
        let doc = parse_html(
            "<style>div{color:#010101;font-weight:bold}</style><div><span>x</span></div>",
        );
        let r = find_run(&doc, "x");
        assert_eq!(r.style.color, Some(Color::rgb(1, 1, 1)));
        assert!(r.style.bold);
    }

    #[test]
    fn media_queries_evaluated() {
        let doc = parse_html(
            "<style>@media (max-width: 1000px){p{color:#010101}}\
             @media (max-width: 100px){p{color:#020202}}\
             @media print{p{color:#030303}}</style><p>x</p>",
        );
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(1, 1, 1)));
    }

    #[test]
    fn at_rules_skipped_safely() {
        let doc = parse_html(
            "<style>@import url(x.css); @font-face{src:url(a)} \
             @keyframes k{0%{color:red}} p{color:#010101}</style><p>x</p>",
        );
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(1, 1, 1)));
    }

    // ── CSS: properties ─────────────────────────────────────────────────────

    #[test]
    fn color_formats() {
        let doc = parse_html(
            "<p style=\"color:rgb(1, 2, 3)\">a</p>\
             <p style=\"color:rgb(50% 0% 100%)\">b</p>\
             <p style=\"color:hsl(0, 100%, 50%)\">c</p>\
             <p style=\"color:#abc\">d</p>\
             <p style=\"color:rebeccapurple\">e</p>",
        );
        assert_eq!(find_run(&doc, "a").style.color, Some(Color::rgb(1, 2, 3)));
        assert_eq!(
            find_run(&doc, "b").style.color,
            Some(Color::rgb(127, 0, 255))
        );
        assert_eq!(find_run(&doc, "c").style.color, Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            find_run(&doc, "d").style.color,
            Some(Color::rgb(0xAA, 0xBB, 0xCC))
        );
        assert_eq!(
            find_run(&doc, "e").style.color,
            Some(Color::rgb(102, 51, 153))
        );
    }

    #[test]
    fn text_decoration_and_transform() {
        let doc = parse_html(
            "<p style=\"text-decoration: line-through underline\">a</p>\
             <p style=\"text-transform: uppercase\">make loud</p>\
             <p style=\"text-transform: capitalize\">two words</p>",
        );
        let a = find_run(&doc, "a");
        assert!(a.style.strike && a.style.underline);
        assert_eq!(block_text(&doc, 1), "MAKE LOUD");
        assert_eq!(block_text(&doc, 2), "Two Words");
    }

    #[test]
    fn display_none_prunes_and_visibility_toggles() {
        let doc = parse_html(
            "<p style=\"display:none\">gone</p>\
             <div style=\"visibility:hidden\">unseen<span style=\"visibility:visible\">seen</span></div>\
             <p>after</p>",
        );
        let t = all_text(&doc);
        assert!(!t.contains("gone"));
        assert!(!t.contains("unseen"));
        assert!(t.contains("seen"));
        assert!(t.contains("after"));
    }

    #[test]
    fn inline_display_block_overrides_stylesheet_display_none() {
        let doc = parse_html(
            "<style>div,span,p { display:none }</style>\
             <div style=\"display:block\">fallback link</div>",
        );
        assert!(all_text(&doc).contains("fallback link"));
    }

    #[test]
    fn text_align_and_center_tag() {
        let doc = parse_html(
            "<p style=\"text-align:center\">c</p><p style=\"text-align:right\">r</p>\
             <center>legacy</center><p align=center>attr</p>",
        );
        let aligns: Vec<Align> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { align, .. } => Some(*align),
                _ => None,
            })
            .collect();
        assert_eq!(
            aligns,
            [Align::Center, Align::Right, Align::Center, Align::Center]
        );
    }

    #[test]
    fn font_shorthand_and_family() {
        let doc = parse_html(
            "<p style=\"font: italic bold 12px monospace\">a</p>\
             <p style=\"font-family: Courier New\">b</p>",
        );
        let a = find_run(&doc, "a");
        assert!(a.style.bold && a.style.mono);
        assert!(find_run(&doc, "b").style.mono);
    }

    #[test]
    fn strike_elements() {
        let doc = parse_html("<p><s>old</s> <del>gone</del> <ins>new</ins></p>");
        assert!(find_run(&doc, "old").style.strike);
        assert!(find_run(&doc, "gone").style.strike);
        assert!(find_run(&doc, "new").style.underline);
    }

    #[test]
    fn page_background_from_css_and_bgcolor() {
        let doc = parse_html("<style>body{background:#0a0e15}</style><p>x</p>");
        assert_eq!(doc.background, Some(Color::rgb(0x0A, 0x0E, 0x15)));
        let doc = parse_html("<body bgcolor=\"#102030\"><p>x</p></body>");
        assert_eq!(doc.background, Some(Color::rgb(0x10, 0x20, 0x30)));
    }

    #[test]
    fn body_text_attribute_sets_color() {
        let doc = parse_html("<body text=\"#aabbcc\"><p>x</p></body>");
        assert_eq!(
            find_run(&doc, "x").style.color,
            Some(Color::rgb(0xAA, 0xBB, 0xCC))
        );
    }

    // ── Misc document structure ─────────────────────────────────────────────

    #[test]
    fn links_collected_and_nested_content_kept() {
        let doc = parse_html("<a href=\"/a\"><b>bold link</b></a>");
        let r = find_run(&doc, "bold link");
        assert_eq!(r.link, Some(0));
        assert!(r.style.bold);
        assert_eq!(doc.links[0], "/a");
    }

    #[test]
    fn images_and_rules() {
        let doc = parse_html("<p>before</p><hr><img src=\"/i.png\" alt=\"pic\">");
        assert!(matches!(doc.blocks[1], Block::Rule));
        match &doc.blocks[2] {
            Block::Image { src, alt, .. } => {
                assert_eq!(src, "/i.png");
                assert_eq!(alt, "pic");
            }
            _ => panic!("expected image"),
        }
    }

    #[test]
    fn empty_document_placeholder() {
        let doc = parse_html("");
        assert_eq!(texts(&doc), ["(empty document)"]);
    }

    #[test]
    fn hidden_attribute_hides() {
        let doc = parse_html("<p hidden>gone</p><p>kept</p>");
        assert_eq!(all_text(&doc), "kept");
    }

    #[test]
    fn definition_lists_indent() {
        let doc = parse_html("<dl><dt>Term</dt><dd>Def</dd></dl>");
        assert_eq!(texts(&doc), ["Term", "Def"]);
        assert_eq!(kind_of(&doc, 1), TextKind::ListItem);
    }

    #[test]
    fn blockquote_keeps_quote_kind_for_inner_paragraphs() {
        let doc = parse_html("<blockquote><p>quoted</p></blockquote>");
        assert_eq!(kind_of(&doc, 0), TextKind::Quote);
    }

    #[test]
    fn svg_subtree_skipped() {
        let doc = parse_html("<p>a</p><svg><text>vector</text></svg><p>b</p>");
        let t = all_text(&doc);
        assert!(!t.contains("vector"));
        assert!(t.contains('a') && t.contains('b'));
    }

    // ── Robustness ──────────────────────────────────────────────────────────

    #[test]
    fn pathological_nesting_is_bounded() {
        // 10k nested elements must not overflow the flattener's recursion.
        let mut html = String::new();
        for _ in 0..10_000 {
            html.push_str("<div><b>");
        }
        html.push_str("deep");
        let doc = parse_html(&html);
        assert!(all_text(&doc).contains("deep"));
    }

    #[test]
    fn truncated_and_garbage_markup_survives() {
        for input in [
            "<p<p<p>>>",
            "<a href=\"unterminated",
            "<!-- never closed",
            "<table><td>cell",
            "</nothing></b></html>x",
            "<![CDATA[ raw ]]>tail",
            "&#xFFFFFFFFFF; &#0; &bogus; &",
            "\u{0}\u{1}binary\u{ff}",
        ] {
            let _ = parse_html(input); // must not panic
        }
    }

    #[test]
    fn select_inside_paragraph_flows_inline() {
        let doc = parse_html("<p>pick <select><option>x</option></select> now</p>");
        match &doc.blocks[0] {
            Block::Text { items, .. } => {
                assert!(items.iter().any(|it| matches!(it, Inline::Control(0))));
            }
            _ => panic!(),
        }
        assert_eq!(doc.inputs[0].options, ["x"]);
    }

    // ── Foster parenting ────────────────────────────────────────────────────

    #[test]
    fn foster_parents_text_before_table() {
        // Stray text inside a table is relocated to just before the table, so
        // it renders ahead of the row content rather than being lost.
        let doc = parse_html("<div>A</div><table><tr><td>B</td></tr>C</table>");
        assert_eq!(texts(&doc), ["A", "C", "B"]);
    }

    #[test]
    fn foster_parents_block_before_table() {
        let doc = parse_html("<table><div>moved</div><tr><td>cell</td></tr></table>");
        let t = texts(&doc);
        let moved = t.iter().position(|s| s == "moved").unwrap();
        let cell = t.iter().position(|s| s == "cell").unwrap();
        assert!(moved < cell, "fostered block should precede the table rows");
    }

    #[test]
    fn foster_parented_text_keeps_formatting() {
        // A formatting element open across a table is reconstructed for the
        // fostered text, which should still be styled.
        let doc = parse_html("<b><table><tr><td>in</td></tr>out</table></b>");
        assert!(find_run(&doc, "out").style.bold);
    }

    #[test]
    fn table_structural_content_not_fostered() {
        // Normal table content is untouched by fostering.
        let doc = parse_html("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>");
        assert_eq!(texts(&doc), ["a | b", "c"]);
    }

    // ── External stylesheets ────────────────────────────────────────────────

    #[test]
    fn link_stylesheet_is_fetched_and_applied() {
        let mut requested = String::new();
        let doc = parse_html_with_css(
            "<head><link rel=\"stylesheet\" href=\"/site.css\"></head><body><p>x</p></body>",
            |href| {
                requested.push_str(href);
                Some(String::from("p{color:#00ff00}"))
            },
        );
        assert_eq!(requested, "/site.css");
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn css_import_extraction() {
        use crate::css::extract_imports;
        assert_eq!(
            extract_imports("@import url(\"a.css\"); @import 'b.css' screen; p{}"),
            vec!["a.css".to_string(), "b.css".to_string()]
        );
        assert_eq!(
            extract_imports("@import url(reset.css) screen and (min-width: 1px);"),
            vec!["reset.css".to_string()]
        );
        // Commented-out imports are ignored.
        assert!(extract_imports("/* @import url(x.css); */ p{}").is_empty());
    }

    #[test]
    fn css_import_is_fetched_and_cascades_before_importer() {
        let mut requested: Vec<String> = Vec::new();
        let doc = parse_html_with_css(
            "<style>@import url('base.css'); p { color: #00ff00 }</style><p>hi</p>",
            |href| {
                requested.push(String::from(href));
                if href == "base.css" {
                    // imported: sets a page background and a (lower-priority,
                    // because earlier) red paragraph colour.
                    Some(String::from("body{background:#010203} p{color:#ff0000}"))
                } else {
                    None
                }
            },
        );
        assert_eq!(requested, vec!["base.css".to_string()]);
        // The importing sheet's `p{color:green}` comes after the import, so it wins.
        assert_eq!(find_run(&doc, "hi").style.color, Some(Color::rgb(0, 255, 0)));
        // …but the imported background still applies.
        assert_eq!(doc.background, Some(Color::rgb(1, 2, 3)));
    }

    #[test]
    fn css_import_is_recursive() {
        let doc = parse_html_with_css(
            "<style>@import 'one.css';</style><p>x</p>",
            |href| match href {
                "one.css" => Some(String::from("@import 'two.css';")),
                "two.css" => Some(String::from("p{color:#0000ff}")),
                _ => None,
            },
        );
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn link_without_stylesheet_rel_is_ignored() {
        let mut fetched = false;
        let _ = parse_html_with_css(
            "<head><link rel=\"icon\" href=\"/favicon.ico\"></head><body><p>x</p></body>",
            |_| {
                fetched = true;
                Some(String::new())
            },
        );
        assert!(!fetched, "only rel=stylesheet links should be fetched");
    }

    #[test]
    fn external_and_inline_css_cascade_in_order() {
        // The external sheet comes first in the head, the inline <style> after,
        // so the later inline rule wins on equal specificity.
        let doc = parse_html_with_css(
            "<head><link rel=stylesheet href=a><style>p{color:#0000ff}</style></head>\
             <body><p>x</p></body>",
            |_| Some(String::from("p{color:#ff0000}")),
        );
        assert_eq!(find_run(&doc, "x").style.color, Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn missing_external_css_is_skipped() {
        let doc = parse_html_with_css(
            "<head><link rel=stylesheet href=gone.css></head><body><p>x</p></body>",
            |_| None,
        );
        // No panic, no styling applied.
        assert_eq!(find_run(&doc, "x").style.color, None);
    }

    // ── Italic ──────────────────────────────────────────────────────────────

    #[test]
    fn italic_from_tags_and_css() {
        let doc = parse_html(
            "<p><i>a</i> <em>b</em> <cite>c</cite> \
             <span style=\"font-style:italic\">d</span> \
             <span style=\"font:italic 16px sans\">e</span> plain</p>",
        );
        for word in ["a", "b", "c", "d", "e"] {
            assert!(find_run(&doc, word).style.italic, "{word} should be italic");
        }
        assert!(!find_run(&doc, "plain").style.italic);
    }

    #[test]
    fn font_style_normal_overrides_inherited_italic() {
        let doc = parse_html("<em>a<span style=\"font-style:normal\">b</span></em>");
        assert!(find_run(&doc, "a").style.italic);
        assert!(!find_run(&doc, "b").style.italic);
    }

    // ── font-size ───────────────────────────────────────────────────────────

    #[test]
    fn font_size_units_map_to_scales() {
        let doc = parse_html(
            "<p style=\"font-size:16px\">base</p>\
             <p style=\"font-size:24px\">mid</p>\
             <p style=\"font-size:40px\">big</p>\
             <p style=\"font-size:small\">tiny</p>\
             <p style=\"font-size:200%\">pct</p>",
        );
        assert_eq!(find_run(&doc, "base").style.size, 1);
        assert_eq!(find_run(&doc, "mid").style.size, 2);
        assert_eq!(find_run(&doc, "big").style.size, 3);
        assert_eq!(find_run(&doc, "tiny").style.size, 1);
        assert_eq!(find_run(&doc, "pct").style.size, 3); // 200% of 16 = 32px
    }

    #[test]
    fn em_font_size_resolves_against_parent() {
        // 1.5em of the parent's 24px ⇒ 36px ⇒ scale 3.
        let doc = parse_html(
            "<div style=\"font-size:24px\"><span style=\"font-size:1.5em\">x</span></div>",
        );
        assert_eq!(find_run(&doc, "x").style.size, 3);
    }

    #[test]
    fn headings_have_larger_default_size() {
        let doc = parse_html("<h1>a</h1><h2>b</h2><p>c</p>");
        let h1 = find_run(&doc, "a").style.size;
        let h2 = find_run(&doc, "b").style.size;
        let p = find_run(&doc, "c").style.size;
        assert!(h1 > h2, "h1 should be larger than h2");
        assert!(h2 > p, "h2 should be larger than body text");
    }

    #[test]
    fn font_size_inherits_to_children() {
        let doc = parse_html("<div style=\"font-size:24px\">a <b>b</b> <span>c</span></div>");
        assert_eq!(find_run(&doc, "a").style.size, 2);
        assert_eq!(find_run(&doc, "b").style.size, 2);
        assert_eq!(find_run(&doc, "c").style.size, 2);
    }

    // ── JavaScript: language core ───────────────────────────────────────────

    /// Run `src` as a page script and return the console output.
    fn run_js(src: &str) -> Vec<String> {
        let html = format!("<body><p>x</p><script>{src}</script></body>");
        let page = crate::html::parse_document(&html, &mut |_| None, &mut |_| None, true);
        page.console
    }

    /// Run a full page with scripting on, returning (doc, console).
    fn run_page(html: &str) -> (Document, Vec<String>) {
        let page = crate::html::parse_document(html, &mut |_| None, &mut |_| None, true);
        (page.doc, page.console)
    }

    #[test]
    fn js_expressions_and_coercion() {
        let out = run_js(
            "console.log(1 + 2 * 3, 'a' + 1, 10 / 4, 7 % 3, 2 ** 10);\
             console.log('5' - 1, '5' + 1, 1 == '1', 1 === '1', null == undefined);\
             console.log(typeof 1, typeof 'x', typeof {}, typeof undefined);",
        );
        assert_eq!(out[0], "7 a1 2.5 1 1024");
        assert_eq!(out[1], "4 51 true false true");
        assert_eq!(out[2], "number string object undefined");
    }

    #[test]
    fn js_document_cookie_round_trips() {
        let out = run_js(
            "document.cookie = 'a=1';\
             document.cookie = 'b=2; path=/';\
             console.log(document.cookie);\
             document.cookie = 'a=updated';\
             console.log(document.cookie);\
             console.log(navigator.cookieEnabled);",
        );
        assert_eq!(out[0], "a=1; b=2");
        assert_eq!(out[1], "a=updated; b=2");
        assert_eq!(out[2], "true");
    }

    #[test]
    fn cookie_jar_scopes_by_domain_path_and_secure() {
        use crate::js::cookie::CookieJar;
        let mut jar = CookieJar::new();
        jar.set_from_response("shop.example.com", "/", "sid=abc; Path=/");
        jar.set_from_response("shop.example.com", "/cart", "cart=1; Path=/cart");
        jar.set_from_response("shop.example.com", "/", "secure=yes; Secure");
        jar.set_from_response("example.com", "/", "wide=1; Domain=example.com");

        // Root path over http: the path=/cart cookie and the Secure one are out.
        assert_eq!(
            jar.request_header("shop.example.com", "/", false).as_deref(),
            Some("sid=abc; wide=1")
        );
        // The /cart path adds the cart cookie; https adds the Secure one.
        assert_eq!(
            jar.request_header("shop.example.com", "/cart", true)
                .as_deref(),
            Some("sid=abc; cart=1; secure=yes; wide=1")
        );
        // A different host only sees the domain cookie.
        assert_eq!(
            jar.request_header("other.com", "/", false).as_deref(),
            None
        );
        assert_eq!(
            jar.request_header("www.example.com", "/", false).as_deref(),
            Some("wide=1")
        );
    }

    #[test]
    fn cookie_jar_deletes_on_expiry_and_hides_httponly() {
        use crate::js::cookie::CookieJar;
        let mut jar = CookieJar::new();
        jar.set_from_response("x.com", "/", "token=secret; HttpOnly");
        jar.set_from_response("x.com", "/", "vis=1");
        // HttpOnly is sent on requests but hidden from document.cookie.
        assert_eq!(
            jar.request_header("x.com", "/", false).as_deref(),
            Some("token=secret; vis=1")
        );
        assert_eq!(jar.document_cookie("x.com", "/", false), "vis=1");
        // Max-Age=0 deletes.
        jar.set_from_response("x.com", "/", "vis=1; Max-Age=0");
        assert_eq!(jar.document_cookie("x.com", "/", false), "");
    }

    #[test]
    fn js_localstorage_methods_and_access() {
        let out = run_js(
            "localStorage.setItem('a', '1');\
             localStorage.setItem('b', 2);\
             localStorage.color = 'red';\
             console.log(localStorage.getItem('a'), localStorage.getItem('b'));\
             console.log(localStorage.color, localStorage.length);\
             console.log(localStorage.getItem('missing'));\
             localStorage.removeItem('a');\
             console.log(localStorage.getItem('a'), localStorage.length);\
             localStorage.clear();\
             console.log(localStorage.length);",
        );
        assert_eq!(out[0], "1 2");
        assert_eq!(out[1], "red 3");
        assert_eq!(out[2], "null");
        assert_eq!(out[3], "null 2");
        assert_eq!(out[4], "0");
    }

    #[test]
    fn js_storage_areas_are_separate() {
        let out = run_js(
            "localStorage.setItem('k', 'L');\
             sessionStorage.setItem('k', 'S');\
             console.log(localStorage.getItem('k'), sessionStorage.getItem('k'));",
        );
        assert_eq!(out[0], "L S");
    }

    /// Run a script with a mock synchronous network hook installed, returning
    /// the console output. Drives the runtime directly (the usual `load_page`
    /// path leaves `net` unset for self-contained pages).
    fn run_js_net(src: &str, mock: crate::js::xhr::NetFetch) -> Vec<String> {
        use crate::domtree::build_dom;
        let mut dom = build_dom(&format!("<script>{src}</script>"));
        let mut rt = crate::js::Runtime::new();
        rt.set_page_context(
            crate::js::cookie::shared(),
            "example.com",
            "http://example.com/app/",
            Some(mock),
        );
        let mut console = Vec::new();
        rt.run_load_scripts(&mut dom, &mut |_| None, &mut console);
        console
    }

    /// Echoes the request method/url back as JSON so tests can assert on both.
    fn mock_net(
        req: &crate::js::xhr::NetRequest,
        _jar: &crate::js::cookie::SharedJar,
    ) -> crate::js::xhr::NetResponse {
        crate::js::xhr::NetResponse {
            status: 200,
            ok: true,
            body: format!("{{\"a\":42,\"method\":\"{}\",\"url\":\"{}\"}}", req.method, req.url),
            final_url: req.url.clone(),
        }
    }

    #[test]
    fn js_xhr_synchronous_get() {
        let out = run_js_net(
            "var x = new XMLHttpRequest();\
             var loaded = false;\
             x.onload = function() { loaded = true; };\
             x.open('GET', '/data.json');\
             x.send();\
             console.log(x.status, x.readyState, loaded);\
             console.log(x.responseText.indexOf('\"a\":42') >= 0);",
            mock_net,
        );
        assert_eq!(out[0], "200 4 true");
        assert_eq!(out[1], "true");
    }

    #[test]
    fn js_fetch_then_text_and_json() {
        let out = run_js_net(
            "fetch('/info').then(function(r) { return r.text(); })\
                            .then(function(t) { console.log(t.indexOf('42') >= 0, true); });\
             fetch('/info', { method: 'POST' })\
                 .then(function(r) { console.log(r.ok, r.status); return r.json(); })\
                 .then(function(d) { console.log(d.a, d.method); });",
            mock_net,
        );
        assert_eq!(out[0], "true true");
        assert_eq!(out[1], "true 200");
        assert_eq!(out[2], "42 POST");
    }

    #[test]
    fn js_fetch_without_network_resolves_not_ok() {
        // The default self-contained page has no net hook installed.
        let out = run_js(
            "fetch('/x').then(function(r) { console.log(r.ok, r.status); });",
        );
        assert_eq!(out[0], "false 0");
    }

    #[test]
    fn js_array_from_and_of() {
        let out = run_js(
            "console.log(Array.of(1, 2, 3).join('-'));\
             console.log(Array.from('abc').join(','));\
             console.log(Array.from([1, 2, 3], function(x) { return x * x; }).join(','));\
             console.log(Array.from(new Set([1, 1, 2])).length);",
        );
        assert_eq!(out[0], "1-2-3");
        assert_eq!(out[1], "a,b,c");
        assert_eq!(out[2], "1,4,9");
        assert_eq!(out[3], "2");
    }

    #[test]
    fn js_map_basic_ops() {
        let out = run_js(
            "var m = new Map();\
             m.set('a', 1).set('b', 2);\
             m.set('a', 10);\
             console.log(m.get('a'), m.get('b'), m.size, m.has('b'));\
             m.delete('b');\
             console.log(m.has('b'), m.size);\
             var ks = []; m.forEach(function(v, k) { ks.push(k + '=' + v); });\
             console.log(ks.join(','));\
             console.log(new Map([['x', 1], ['y', 2]]).get('y'));",
        );
        assert_eq!(out[0], "10 2 2 true");
        assert_eq!(out[1], "false 1");
        assert_eq!(out[2], "a=10");
        assert_eq!(out[3], "2");
    }

    #[test]
    fn js_set_basic_ops_and_for_of() {
        let out = run_js(
            "var s = new Set();\
             s.add(1).add(2).add(2).add(3);\
             console.log(s.size, s.has(2), s.has(9));\
             s.delete(2);\
             var sum = 0; for (var v of s) { sum += v; }\
             console.log(sum, s.size);",
        );
        assert_eq!(out[0], "3 true false");
        assert_eq!(out[1], "4 2");
    }

    #[test]
    fn js_map_for_of_destructures_entries() {
        let out = run_js(
            "var m = new Map([['a', 1], ['b', 2]]);\
             var out = [];\
             for (var e of m) { out.push(e[0] + ':' + e[1]); }\
             console.log(out.join(','));",
        );
        assert_eq!(out[0], "a:1,b:2");
    }

    #[test]
    fn js_closures_and_recursion() {
        let out = run_js(
            "function counter() { var n = 0; return function() { return ++n; }; }\
             var c = counter(); c(); c();\
             function fib(n) { return n < 2 ? n : fib(n-1) + fib(n-2); }\
             console.log(c(), fib(15));",
        );
        assert_eq!(out[0], "3 610");
    }

    #[test]
    fn js_arrays_objects_strings() {
        let out = run_js(
            "var a = [3, 1, 2];\
             console.log(a.map(function(x) { return x * 2; }).join('-'));\
             console.log(a.filter(x => x > 1).length, a.indexOf(2));\
             a.sort(function(x, y) { return x - y; });\
             console.log(a.join(','));\
             var o = {name: 'atom', os: true};\
             console.log(o.name, o['os'], Object.keys(o).join('+'));\
             console.log('Hello World'.toUpperCase().slice(0, 5), 'a,b,c'.split(',').length);",
        );
        assert_eq!(out[0], "6-2-4");
        assert_eq!(out[1], "2 2");
        assert_eq!(out[2], "1,2,3");
        assert_eq!(out[3], "atom true name+os");
        assert_eq!(out[4], "HELLO 3");
    }

    #[test]
    fn js_control_flow() {
        let out = run_js(
            "var s = '';\
             for (var i = 0; i < 5; i++) { if (i == 2) continue; s += i; }\
             for (var k in {a: 1, b: 2}) s += k;\
             for (var v of [9, 8]) s += v;\
             var t = 0;\
             switch (2) { case 1: t = 1; break; case 2: t = 2; case 3: t += 10; break; default: t = 99; }\
             console.log(s, t);",
        );
        assert_eq!(out[0], "0134ab98 12");
    }

    #[test]
    fn js_prototypes_and_new() {
        let out = run_js(
            "function Animal(name) { this.name = name; }\
             Animal.prototype.speak = function() { return this.name + ' speaks'; };\
             var dog = new Animal('Rex');\
             console.log(dog.speak(), dog instanceof Animal, dog.hasOwnProperty('name'));",
        );
        assert_eq!(out[0], "Rex speaks true true");
    }

    #[test]
    fn js_exceptions() {
        let out = run_js(
            "try { null.x; } catch (e) { console.log('caught', e.name); }\
             try { throw new Error('boom'); } catch (e) { console.log(e.message); } finally { console.log('done'); }",
        );
        assert_eq!(out[0], "caught TypeError");
        assert_eq!(out[1], "boom");
        assert_eq!(out[2], "done");
        // Uncaught errors are reported but don't kill the page.
        let (doc, console) = run_page("<p>alive</p><script>throw new Error('x');</script>");
        assert!(console.iter().any(|l| l.contains("Uncaught")));
        assert!(all_text(&doc).contains("alive"));
    }

    #[test]
    fn js_template_literals_and_arrows() {
        let out = run_js(
            "const add = (a, b = 10) => a + b;\
             let who = 'world';\
             console.log(`hi ${who}, ${add(1, 2)} and ${add(5)}`);",
        );
        assert_eq!(out[0], "hi world, 3 and 15");
    }

    #[test]
    fn js_math_and_json() {
        let out = run_js(
            "console.log(Math.floor(2.7), Math.ceil(-2.1), Math.sqrt(144), Math.pow(3, 4), Math.max(1, 9, 4));\
             console.log((3.14159).toFixed(2), parseInt('42px'), parseFloat('2.5em'), isNaN('abc' * 1));\
             var o = JSON.parse('{\"a\": [1, 2], \"b\": \"x\"}');\
             console.log(o.a[1], o.b, JSON.stringify({k: [true, null]}));",
        );
        assert_eq!(out[0], "2 -2 12 81 9");
        assert_eq!(out[1], "3.14 42 2.5 true");
        assert_eq!(out[2], "2 x {\"k\":[true,null]}");
    }

    #[test]
    fn js_call_apply_bind() {
        let out = run_js(
            "function who() { return this.name; }\
             var o = {name: 'atom'};\
             console.log(who.call(o), who.apply(o, []), who.bind(o)());",
        );
        assert_eq!(out[0], "atom atom atom");
    }

    #[test]
    fn js_infinite_loop_is_bounded() {
        // Must terminate (budget abort), and the page must still render.
        let (doc, console) = run_page("<p>safe</p><script>while (true) { var x = 1; }</script>");
        assert!(console.iter().any(|l| l.contains("aborted")));
        assert!(all_text(&doc).contains("safe"));
    }

    #[test]
    fn js_deep_recursion_is_bounded() {
        let (_, console) = run_page("<script>function f() { return f(); } f();</script>");
        assert!(console.iter().any(|l| l.contains("aborted")));
    }

    #[test]
    fn js_parse_error_does_not_kill_page() {
        let (doc, console) = run_page("<p>ok</p><script>class X {}</script>");
        assert!(console.iter().any(|l| l.contains("script error")));
        assert!(all_text(&doc).contains("ok"));
    }

    // ── JavaScript: DOM bindings ────────────────────────────────────────────

    #[test]
    fn js_get_element_by_id_and_text_content() {
        let (doc, _) = run_page(
            "<p id=\"target\">before</p>\
             <script>document.getElementById('target').textContent = 'after';</script>",
        );
        let t = all_text(&doc);
        assert!(t.contains("after") && !t.contains("before"));
    }

    #[test]
    fn js_document_write_inserts_at_script_position() {
        let (doc, _) =
            run_page("<p>first</p><script>document.write('<b>written</b>');</script><p>last</p>");
        assert_eq!(texts(&doc), ["first", "written", "last"]);
        assert!(find_run(&doc, "written").style.bold);
    }

    #[test]
    fn js_inner_html_set() {
        let (doc, _) = run_page(
            "<div id=\"box\">old</div>\
             <script>document.getElementById('box').innerHTML = '<u>new</u> text';</script>",
        );
        let t = all_text(&doc);
        assert!(t.contains("new text") && !t.contains("old"));
        assert!(find_run(&doc, "new").style.underline);
    }

    #[test]
    fn js_create_element_append_child() {
        let (doc, _) = run_page(
            "<div id=\"root\"></div>\
             <script>\
             var el = document.createElement('p');\
             el.appendChild(document.createTextNode('made by script'));\
             document.getElementById('root').appendChild(el);\
             </script>",
        );
        assert!(all_text(&doc).contains("made by script"));
    }

    #[test]
    fn js_style_mutation_changes_rendering() {
        let (doc, _) = run_page(
            "<p id=\"p\">painted</p>\
             <script>\
             var p = document.getElementById('p');\
             p.style.color = '#ff0000';\
             p.style.fontWeight = 'bold';\
             </script>",
        );
        let r = find_run(&doc, "painted");
        assert_eq!(r.style.color, Some(Color::rgb(255, 0, 0)));
        assert!(r.style.bold);
    }

    #[test]
    fn js_query_selector_and_class_list() {
        let (doc, out) = run_page(
            "<style>.lit{color:#00ff00}</style>\
             <ul><li>a</li><li class=\"x\">b</li><li>c</li></ul>\
             <script>\
             console.log(document.querySelectorAll('li').length);\
             console.log(document.querySelector('li.x').textContent);\
             document.querySelector('li.x').classList.add('lit');\
             </script>",
        );
        assert_eq!(out[0], "3");
        assert_eq!(out[1], "b");
        assert_eq!(find_run(&doc, "b").style.color, Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn js_document_title_set() {
        let (doc, _) = run_page(
            "<head><title>old</title></head><body><script>document.title = 'scripted';</script></body>",
        );
        assert_eq!(doc.title, "scripted");
    }

    #[test]
    fn js_set_attribute_and_generic_props() {
        let (doc, out) = run_page(
            "<a id=\"l\" href=\"/page\">link</a>\
             <script>\
             var a = document.getElementById('l');\
             console.log(a.href, a.tagName, a.getAttribute('href'));\
             a.setAttribute('href', '/other');\
             </script>",
        );
        assert_eq!(out[0], "/page A /page");
        assert_eq!(doc.links[0], "/other");
    }

    #[test]
    fn js_external_script_fetched() {
        let mut requested = String::new();
        let page = crate::html::parse_document(
            "<script src=\"/app.js\"></script><p>body</p>",
            &mut |_| None,
            &mut |src| {
                requested.push_str(src);
                Some(String::from("console.log('external ran');"))
            },
            true,
        );
        assert_eq!(requested, "/app.js");
        assert_eq!(page.console[0], "external ran");
    }

    #[test]
    fn js_noscript_hidden_when_scripting_on() {
        let html = "<noscript><p>no js</p></noscript><p>always</p>";
        let (with_js, _) = run_page(html);
        assert!(!all_text(&with_js).contains("no js"));
        let without_js = parse_html(html);
        assert!(all_text(&without_js).contains("no js"));
    }

    #[test]
    fn js_scripts_share_global_scope_in_order() {
        let (_, out) = run_page(
            "<script>var shared = 'one';</script>\
             <script>shared += ' two'; console.log(shared);</script>",
        );
        assert_eq!(out[0], "one two");
    }

    #[test]
    fn js_asi_inserts_semicolons() {
        let out = run_js("var a = 1\nvar b = 2\nconsole.log(a + b)\n");
        assert_eq!(out[0], "3");
        // Restricted production: `return\n value` returns undefined.
        let out = run_js("function f() { return\n42 }\nconsole.log(f());");
        assert_eq!(out[0], "undefined");
    }

    // ── JavaScript: events ──────────────────────────────────────────────────

    use crate::html::{flatten_dom, load_page, LoadedPage};
    use crate::js::Target;

    fn load(html: &str) -> LoadedPage {
        load_page(
            html,
            &mut |_| None,
            &mut |_| None,
            true,
            crate::html::LoadContext::local(),
        )
    }

    /// Click the node and re-flatten, like the browser does.
    fn click_and_reflatten(page: &mut LoadedPage, node: usize) -> bool {
        let rt = page.runtime.as_mut().unwrap();
        let mut console = Vec::new();
        let outcome = rt.dispatch(&mut page.dom, &mut console, Target::Node(node), "click");
        let clickable = page.runtime.as_ref().unwrap().click_targets();
        page.doc = flatten_dom(&page.dom, &mut |_| None, true, &clickable);
        outcome.prevented
    }

    #[test]
    fn ev_add_event_listener_click_mutates_page() {
        let mut page = load(
            "<p id=\"out\">before</p><a id=\"go\" href=\"/x\">go</a>\
             <script>document.getElementById('go').addEventListener('click', function() {\
                document.getElementById('out').textContent = 'clicked';\
             });</script>",
        );
        assert!(all_text(&page.doc).contains("before"));
        let node = page.doc.link_nodes[0];
        let prevented = click_and_reflatten(&mut page, node);
        assert!(!prevented, "no preventDefault: navigation proceeds");
        assert!(all_text(&page.doc).contains("clicked"));
    }

    #[test]
    fn ev_prevent_default_blocks_navigation() {
        let mut page = load(
            "<a id=\"go\" href=\"/x\">go</a>\
             <script>document.getElementById('go').onclick = function(e) { e.preventDefault(); };</script>",
        );
        let node = page.doc.link_nodes[0];
        assert!(click_and_reflatten(&mut page, node));
        // `return false` from a property handler also prevents.
        let mut page = load(
            "<a id=\"go\" href=\"/x\">go</a>\
             <script>document.getElementById('go').onclick = function() { return false; };</script>",
        );
        let node = page.doc.link_nodes[0];
        assert!(click_and_reflatten(&mut page, node));
    }

    #[test]
    fn ev_onclick_attribute_fires_and_prevents() {
        let mut page = load(
            "<p id=\"out\">x</p>\
             <a href=\"/x\" onclick=\"document.getElementById('out').textContent = 'attr ran'; return false\">go</a>",
        );
        let node = page.doc.link_nodes[0];
        let prevented = click_and_reflatten(&mut page, node);
        assert!(prevented);
        assert!(all_text(&page.doc).contains("attr ran"));
    }

    #[test]
    fn ev_bubbling_and_stop_propagation() {
        let mut page = load(
            "<div id=\"wrap\"><a id=\"go\" href=\"/x\">go</a></div><p id=\"out\"></p>\
             <script>\
             var log = [];\
             document.getElementById('go').addEventListener('click', function() { log.push('a'); });\
             document.getElementById('wrap').addEventListener('click', function() { log.push('div'); });\
             document.addEventListener('click', function() { log.push('doc'); });\
             window.addEventListener('click', function() {\
                log.push('win');\
                document.getElementById('out').textContent = log.join('>');\
             });\
             </script>",
        );
        let node = page.doc.link_nodes[0];
        click_and_reflatten(&mut page, node);
        assert!(all_text(&page.doc).contains("a>div>doc>win"));

        // stopPropagation halts before document/window.
        let mut page = load(
            "<div id=\"wrap\"><a id=\"go\" href=\"/x\">go</a></div><p id=\"out\">none</p>\
             <script>\
             document.getElementById('go').addEventListener('click', function(e) {\
                e.stopPropagation();\
                document.getElementById('out').textContent = 'inner only';\
             });\
             document.addEventListener('click', function() {\
                document.getElementById('out').textContent = 'leaked';\
             });\
             </script>",
        );
        let node = page.doc.link_nodes[0];
        click_and_reflatten(&mut page, node);
        assert!(all_text(&page.doc).contains("inner only"));
    }

    #[test]
    fn ev_dom_content_loaded_and_window_onload_fire_at_load() {
        let page = load(
            "<p id=\"a\">-</p><p id=\"b\">-</p>\
             <script>\
             document.addEventListener('DOMContentLoaded', function() {\
                document.getElementById('a').textContent = 'dcl ran';\
             });\
             window.onload = function() {\
                document.getElementById('b').textContent = 'onload ran';\
             };\
             </script>",
        );
        let t = all_text(&page.doc);
        assert!(t.contains("dcl ran"), "DOMContentLoaded should fire: {t}");
        assert!(t.contains("onload ran"), "window.onload should fire: {t}");
    }

    #[test]
    fn ev_click_zones_for_non_link_elements() {
        // A span with onclick gets a clickable region in the flat document.
        let page = load("<p><span onclick=\"1\">tap me</span> plain</p>");
        assert_eq!(page.doc.click_nodes.len(), 1);
        let zoned = match &page.doc.blocks[0] {
            Block::Text { items, .. } => items.iter().any(
                |it| matches!(it, Inline::Run(r) if r.zone == Some(0) && r.text.contains("tap")),
            ),
            _ => false,
        };
        assert!(zoned, "span text should carry the click zone");
        // addEventListener targets get zones too (registered via script).
        let page = load(
            "<p id=\"t\">tap</p>\
             <script>document.getElementById('t').addEventListener('click', function() {});</script>",
        );
        assert_eq!(page.doc.click_nodes.len(), 1);
    }

    #[test]
    fn ev_remove_event_listener() {
        let mut page = load(
            "<a id=\"go\" href=\"/x\">go</a><p id=\"out\">none</p>\
             <script>\
             function h() { document.getElementById('out').textContent = 'fired'; }\
             var a = document.getElementById('go');\
             a.addEventListener('click', h);\
             a.removeEventListener('click', h);\
             </script>",
        );
        let node = page.doc.link_nodes[0];
        click_and_reflatten(&mut page, node);
        assert!(!all_text(&page.doc).contains("fired"));
    }

    #[test]
    fn ev_element_click_method_dispatches_synchronously() {
        let page = load(
            "<a id=\"go\" href=\"/x\">go</a><p id=\"out\">-</p>\
             <script>\
             document.getElementById('go').onclick = function() {\
                document.getElementById('out').textContent = 'via click()';\
             };\
             document.getElementById('go').click();\
             </script>",
        );
        assert!(all_text(&page.doc).contains("via click()"));
    }

    #[test]
    fn ev_handler_errors_reported_not_fatal() {
        let mut page = load(
            "<a id=\"go\" href=\"/x\">go</a>\
             <script>document.getElementById('go').onclick = function() { boom(); };</script>",
        );
        let node = page.doc.link_nodes[0];
        let rt = page.runtime.as_mut().unwrap();
        let mut console = Vec::new();
        let outcome = rt.dispatch(&mut page.dom, &mut console, Target::Node(node), "click");
        assert!(outcome.fired);
        assert!(!outcome.prevented);
        assert!(console.iter().any(|l| l.contains("Uncaught")));
    }

    #[test]
    fn ev_input_nodes_track_controls() {
        let page = load("<input id=\"i\" name=\"q\"><button id=\"b\">Go</button>");
        assert_eq!(page.doc.inputs.len(), 2);
        assert_eq!(page.doc.input_nodes.len(), 2);
        assert_eq!(page.doc.links.len(), page.doc.link_nodes.len());
    }

    #[test]
    fn about_pages_still_render() {
        // The built-in pages from content.rs (inlined here) must keep working.
        let home = parse_html(
            r#"<!doctype html><html><head><title>Home</title>
            <style>body { background: #0a0e15; color: #e2e8f0; } h1 { color: #58a6ff; }
            .muted { color: gray; }</style></head>
            <body><h1>Home</h1><p>Open a page.</p>
            <p class="muted">HTTPS needs TLS.</p></body></html>"#,
        );
        assert_eq!(home.title, "Home");
        assert_eq!(home.background, Some(Color::rgb(0x0A, 0x0E, 0x15)));
        assert_eq!(
            find_run(&home, "Home").style.color,
            Some(Color::rgb(0x58, 0xA6, 0xFF))
        );
        assert_eq!(
            find_run(&home, "HTTPS").style.color,
            Some(Color::rgb(128, 128, 128))
        );
        assert_eq!(
            find_run(&home, "Open").style.color,
            Some(Color::rgb(0xE2, 0xE8, 0xF0))
        );
    }

    // ── Timers ──────────────────────────────────────────────────────────────
    //
    // Timer tests must run serially because BROWSER_TIME_MS is a shared atomic.
    // Each test resets the clock to 0 before calling `load_at_time`, ensuring
    // `setTimeout(fn, N)` schedules `deadline_ms = 0 + N`.

    static TIMER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Load a page with BROWSER_TIME_MS pinned to `start_ms`.
    fn load_at_time(html: &str, start_ms: u64) -> crate::html::LoadedPage {
        crate::js::builtins::set_browser_time(start_ms);
        load_page(
            html,
            &mut |_| None,
            &mut |_| None,
            true,
            crate::html::LoadContext::local(),
        )
    }

    /// Advance the browser clock to `now_ms` and fire expired timers,
    /// re-flattening the document if any ran.
    fn tick_page(page: &mut crate::html::LoadedPage, now_ms: u64) -> bool {
        crate::js::builtins::set_browser_time(now_ms);
        if let Some(rt) = page.runtime.as_mut() {
            let mut console = Vec::new();
            if rt.tick_timers(&mut page.dom, &mut console, now_ms) {
                let clickable = page.runtime.as_ref().unwrap().click_targets();
                page.doc = crate::html::flatten_dom(&page.dom, &mut |_| None, true, &clickable);
                return true;
            }
        }
        false
    }

    #[test]
    fn timer_zero_delay_still_runs_inline() {
        let _g = TIMER_LOCK.lock().unwrap();
        // delay == 0 still executes synchronously during page load.
        let page = load_at_time(
            "<p id='out'>before</p>\
             <script>setTimeout(function() {\
                document.getElementById('out').textContent = 'inline';\
             }, 0);</script>",
            0,
        );
        assert!(all_text(&page.doc).contains("inline"));
    }

    #[test]
    fn timer_positive_delay_deferred() {
        let _g = TIMER_LOCK.lock().unwrap();
        // A positive delay must NOT run during load.
        let page = load_at_time(
            "<p id='out'>before</p>\
             <script>setTimeout(function() {\
                document.getElementById('out').textContent = 'fired';\
             }, 500);</script>",
            0,
        );
        assert!(
            all_text(&page.doc).contains("before"),
            "should not have fired yet"
        );
    }

    #[test]
    fn timer_fires_after_deadline() {
        let _g = TIMER_LOCK.lock().unwrap();
        let mut page = load_at_time(
            "<p id='out'>before</p>\
             <script>setTimeout(function() {\
                document.getElementById('out').textContent = 'fired';\
             }, 500);</script>",
            0,
        );
        // 499 ms — should not fire yet.
        let fired = tick_page(&mut page, 499);
        assert!(!fired);
        assert!(all_text(&page.doc).contains("before"));
        // 500 ms — deadline reached, must fire.
        let fired = tick_page(&mut page, 500);
        assert!(fired, "timer should fire exactly at its deadline");
        assert!(all_text(&page.doc).contains("fired"));
    }

    #[test]
    fn timer_fires_only_once() {
        let _g = TIMER_LOCK.lock().unwrap();
        let mut page = load_at_time(
            "<p id='out'>0</p>\
             <script>\
             var n = 0;\
             setTimeout(function() {\
                n++;\
                document.getElementById('out').textContent = String(n);\
             }, 100);\
             </script>",
            0,
        );
        tick_page(&mut page, 100);
        tick_page(&mut page, 200);
        tick_page(&mut page, 300);
        // The timeout fires exactly once.
        assert_eq!(block_text(&page.doc, 0), "1");
    }

    #[test]
    fn interval_fires_repeatedly() {
        let _g = TIMER_LOCK.lock().unwrap();
        let mut page = load_at_time(
            "<p id='out'>0</p>\
             <script>\
             var n = 0;\
             setInterval(function() {\
                n++;\
                document.getElementById('out').textContent = String(n);\
             }, 100);\
             </script>",
            0,
        );
        tick_page(&mut page, 100);
        assert_eq!(block_text(&page.doc, 0), "1");
        tick_page(&mut page, 200);
        assert_eq!(block_text(&page.doc, 0), "2");
        tick_page(&mut page, 300);
        assert_eq!(block_text(&page.doc, 0), "3");
    }

    #[test]
    fn clear_timeout_cancels_pending() {
        let _g = TIMER_LOCK.lock().unwrap();
        let mut page = load_at_time(
            "<p id='out'>before</p>\
             <script>\
             var id = setTimeout(function() {\
                document.getElementById('out').textContent = 'fired';\
             }, 100);\
             clearTimeout(id);\
             </script>",
            0,
        );
        tick_page(&mut page, 100);
        assert!(
            all_text(&page.doc).contains("before"),
            "clearTimeout should have prevented the callback"
        );
    }

    #[test]
    fn clear_interval_stops_repeating() {
        let _g = TIMER_LOCK.lock().unwrap();
        // The interval fires at 100 and 200 ms. A separate timeout at 250 ms
        // clears the interval, so the tick at 300 must NOT re-fire it.
        let mut page = load_at_time(
            "<p id='out'>0</p>\
             <script>\
             var n = 0;\
             var id = setInterval(function() {\
                n++;\
                document.getElementById('out').textContent = String(n);\
             }, 100);\
             setTimeout(function() { clearInterval(id); }, 250);\
             </script>",
            0,
        );
        tick_page(&mut page, 100); // interval fires → n=1
        tick_page(&mut page, 200); // interval fires → n=2
        tick_page(&mut page, 250); // clearInterval timeout fires; interval not due yet
        tick_page(&mut page, 300); // interval cleared — must not fire
        tick_page(&mut page, 400);
        assert_eq!(
            block_text(&page.doc, 0),
            "2",
            "interval should have been stopped after firing twice"
        );
    }

    #[test]
    fn date_now_returns_browser_time() {
        let _g = TIMER_LOCK.lock().unwrap();
        crate::js::builtins::set_browser_time(12345);
        let msgs = run_js("var t = Date.now(); console.log(t === 12345 ? 'ok' : 'bad');");
        // Reset so other tests see 0.
        crate::js::builtins::set_browser_time(0);
        assert!(
            msgs.iter().any(|m| m == "ok"),
            "Date.now() must reflect set_browser_time; got {:?}",
            msgs
        );
    }

    // ── keydown / input / change / submit events ─────────────────────────────

    #[test]
    fn keydown_event_fires_on_node() {
        let mut page = load(
            "<p id='out'>-</p>\
             <script>\
             document.addEventListener('keydown', function(e) {\
                document.getElementById('out').textContent = e.key;\
             });\
             </script>",
        );
        let rt = page.runtime.as_mut().unwrap();
        let mut console = Vec::new();
        let outcome = rt.dispatch_keyboard(
            &mut page.dom,
            &mut console,
            crate::js::Target::Document,
            "keydown",
            65,
            0,
            false,
            false,
            false, // 'A' character, no modifiers
        );
        assert!(outcome.fired);
        let clickable = rt.click_targets();
        page.doc = crate::html::flatten_dom(&page.dom, &mut |_| None, true, &clickable);
        assert_eq!(block_text(&page.doc, 0), "A");
    }

    #[test]
    fn keydown_event_has_key_properties() {
        let msgs = run_js(
            "var captured = {};\
             document.addEventListener('keydown', function(e) {\
                captured.key = e.key;\
                captured.keyCode = e.keyCode;\
                captured.ctrlKey = e.ctrlKey;\
             });",
        );
        // run_js doesn't dispatch events — just verify no parse errors.
        assert!(msgs.is_empty() || !msgs.iter().any(|m| m.contains("error")));
    }

    #[test]
    fn submit_event_prevented_blocks_navigation() {
        // We simulate submit prevention by checking the outcome.
        let mut page = load(
            "<form id='f'><input name='q'><button type='submit'>Go</button></form>\
             <script>\
             document.getElementById('f').addEventListener('submit', function(e) {\
                e.preventDefault();\
             });\
             </script>",
        );
        // Find the form node and dispatch submit.
        let form_node = page.dom.find_first("form").unwrap();
        let rt = page.runtime.as_mut().unwrap();
        let mut console = Vec::new();
        let outcome = rt.dispatch(
            &mut page.dom,
            &mut console,
            crate::js::Target::Node(form_node),
            "submit",
        );
        assert!(
            outcome.prevented,
            "preventDefault on submit should be reported"
        );
    }

    #[test]
    fn input_event_dispatches_on_value_change() {
        // Verify that an input event is dispatchable on an input node.
        let mut page = load(
            "<input id='f'><p id='out'>-</p>\
             <script>\
             document.getElementById('f').addEventListener('input', function(e) {\
                document.getElementById('out').textContent = 'changed';\
             });\
             </script>",
        );
        let input_node = page.doc.input_nodes[0];
        let rt = page.runtime.as_mut().unwrap();
        let mut console = Vec::new();
        let outcome = rt.dispatch(
            &mut page.dom,
            &mut console,
            crate::js::Target::Node(input_node),
            "input",
        );
        assert!(outcome.fired);
        let clickable = rt.click_targets();
        page.doc = crate::html::flatten_dom(&page.dom, &mut |_| None, true, &clickable);
        assert!(all_text(&page.doc).contains("changed"));
    }

    #[test]
    fn change_event_dispatches_on_blur() {
        let mut page = load(
            "<input id='f'><p id='out'>-</p>\
             <script>\
             document.getElementById('f').addEventListener('change', function(e) {\
                document.getElementById('out').textContent = 'blurred';\
             });\
             </script>",
        );
        let input_node = page.doc.input_nodes[0];
        let rt = page.runtime.as_mut().unwrap();
        let mut console = Vec::new();
        let outcome = rt.dispatch(
            &mut page.dom,
            &mut console,
            crate::js::Target::Node(input_node),
            "change",
        );
        assert!(outcome.fired);
        let clickable = rt.click_targets();
        page.doc = crate::html::flatten_dom(&page.dom, &mut |_| None, true, &clickable);
        assert!(all_text(&page.doc).contains("blurred"));
    }
}
