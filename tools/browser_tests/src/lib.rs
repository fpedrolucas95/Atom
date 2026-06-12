//! Host-side regression tests for the browser's HTML5 + CSS engine.
//!
//! The engine modules are `no_std + alloc` and depend only on `libgui::color`
//! and `libimage` types, so they compile unchanged on the host against the
//! stub crates in `stubs/`. Run with `cargo test` in this directory.

extern crate alloc;

#[path = "../../../userspace/apps/browser/src/text.rs"]
pub mod text;
#[path = "../../../userspace/apps/browser/src/entities.rs"]
pub mod entities;
#[path = "../../../userspace/apps/browser/src/tokenizer.rs"]
pub mod tokenizer;
#[path = "../../../userspace/apps/browser/src/domtree.rs"]
pub mod domtree;
#[path = "../../../userspace/apps/browser/src/dom.rs"]
pub mod dom;
#[path = "../../../userspace/apps/browser/src/css.rs"]
pub mod css;
#[path = "../../../userspace/apps/browser/src/style.rs"]
pub mod style;
#[path = "../../../userspace/apps/browser/src/html.rs"]
pub mod html;
#[path = "../../../userspace/apps/browser/src/js/mod.rs"]
pub mod js;

#[cfg(test)]
mod tests {
    use crate::dom::{Align, Block, Document, Inline, InputKind, TextKind};
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
        let doc = parse_html(
            "<style>p{}</style><script>var a = '<p>nope</p>';</script><p>shown</p>",
        );
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
        let doc = parse_html(
            "<style>.a{color:#ff0000}.b{color:#00ff00}</style><p class=\"a b\">x</p>",
        );
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
        let doc = parse_html(
            "<style>p:not(.skip){color:#010101}</style><p>yes</p><p class=skip>no</p>",
        );
        assert_eq!(find_run(&doc, "yes").style.color, Some(Color::rgb(1, 1, 1)));
        assert_eq!(find_run(&doc, "no").style.color, None);
    }

    #[test]
    fn hover_never_matches_but_parses() {
        let doc = parse_html(
            "<style>a:hover{color:#010101} a{color:#020202}</style><a href=x>l</a>",
        );
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
                assert!(items
                    .iter()
                    .any(|it| matches!(it, Inline::Control(0))));
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
        let doc = parse_html(
            "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>",
        );
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
        let doc = parse_html(
            "<em>a<span style=\"font-style:normal\">b</span></em>",
        );
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
        let doc = parse_html(
            "<div style=\"font-size:24px\">a <b>b</b> <span>c</span></div>",
        );
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
        let (doc, _) = run_page(
            "<p>first</p><script>document.write('<b>written</b>');</script><p>last</p>",
        );
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
        load_page(html, &mut |_| None, &mut |_| None, true)
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
            Block::Text { items, .. } => items.iter().any(|it| {
                matches!(it, Inline::Run(r) if r.zone == Some(0) && r.text.contains("tap"))
            }),
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
}
