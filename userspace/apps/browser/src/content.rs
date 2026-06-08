//! Built-in `about:` pages served without touching the network.

pub const ABOUT_HOME: &str = r#"
<!doctype html>
<html>
<head><title>Atom Browser</title></head>
<body>
  <h1>Atom Browser</h1>
  <p>A small userspace browser for Atom OS. Type a URL above and press Enter, or click a link below.</p>
  <h2>Check your connection</h2>
  <p>Try <a href="http://neverssl.com/">neverssl</a> or <a href="http://example.com/">example</a> to load a plain-HTTP page through netd.</p>
  <p>Note: HTTPS sites (such as google.com) are not supported yet because Atom has no TLS stack.</p>
  <p>Open <a href="about:html">about:html</a> for a richer render test (formatting, lists, CSS, images, search box).</p>
</body>
</html>
"#;

pub const ABOUT_HTML: &str = r#"
<!doctype html>
<html>
<head>
  <title>HTML Demo</title>
  <style>
    h2 { color: #2684ff; }
    .note { color: gray; }
    .danger { color: #d63031; font-weight: bold; }
  </style>
</head>
<body>
  <h1>Render Test</h1>
  <p>This page exercises whitespace collapsing, entity decoding like &amp;, &lt;, &copy; and &mdash;, and wrapping across the content area.</p>
  <h2>Inline formatting</h2>
  <p>Text can be <b>bold</b>, <code>monospaced</code>, <u>underlined</u>, or
     <span style="color:#16a34a">coloured via inline CSS</span>. The
     <span class="danger">danger class</span> is styled from the stylesheet.</p>
  <h2>Links</h2>
  <p>Visit <a href="http://example.com/">Example Domain</a> or <a href="http://neverssl.com/">NeverSSL</a>.</p>
  <hr>
  <h2>Search box</h2>
  <form action="http://example.com/search">
    <input type="search" name="q" placeholder="Type a query and press Enter">
    <input type="submit" value="Search">
  </form>
  <h2>Lists</h2>
  <ol>
    <li>First ordered item, numbered automatically.</li>
    <li>Second item with an entity: Atom &amp; HTML.</li>
  </ol>
  <ul>
    <li>Bullet item with enough words to wrap onto a second line in the viewport.</li>
  </ul>
  <h2>Blockquote</h2>
  <blockquote>The best way to predict the future is to invent it.</blockquote>
  <h2>Images (PNG / JPEG / GIF)</h2>
  <p>The GIF below is decoded from an inline <code>data:</code> URI:</p>
  <img alt="demo gif" src="data:image/gif;base64,R0lGODlhDgAOAIEAAP///yaE/yLFXgAAACwAAAAADgAOAAAIPQADCARAUGAAggAMDiy4MKFChAYJCmgYkeFBhA4vZsS4EcDEhRU7ZqRIsuJHjSUhNrSoUqPHkBVhlkzJMCAAOw==">
  <p class="note">Numeric entities work too: &#65;&#66;&#67; and &#x2192; arrows.</p>
</body>
</html>
"#;
