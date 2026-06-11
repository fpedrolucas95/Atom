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

#[cfg(test)]
mod tests {
    use crate::dom::{Align, Block, Document, Inline, InputKind, TextKind};
    use crate::html::parse_html;
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
