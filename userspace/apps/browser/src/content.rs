//! Built-in `about:` pages served without touching the network.

pub const ABOUT_LOADING: &str = r#"
<!doctype html>
<html>
<head><title>Loading</title><style>body { background: #0a0e15; color: #e2e8f0; }</style></head>
<body>
  <h1>Loading</h1>
  <p>Opening the page and preparing resources...</p>
</body>
</html>
"#;

pub const ABOUT_HOME: &str = r#"
<!doctype html>
<html>
<head>
  <title>Home</title>
  <style>
    body { background: #0a0e15; color: #e2e8f0; }
    h1 { color: #58a6ff; }
    h2 { color: #d0d8e6; }
    .muted { color: gray; }
  </style>
</head>
<body>
  <h1>Home</h1>
  <p>Open a plain HTTP site from the address bar, or start with one of these pages.</p>
  <h2>Quick links</h2>
  <p><a href="http://neverssl.com/">NeverSSL</a>   <a href="http://example.com/">Example</a>   <a href="about:html">Render lab</a></p>
  <h2>Notes</h2>
  <p class="muted">HTTPS still requires a TLS stack, so pages that force HTTPS may show an error.</p>
</body>
</html>
"#;

pub const ABOUT_HTML: &str = r#"
<!doctype html>
<html>
<head>
  <title>HTML Demo</title>
  <style>
    body { background: #0a0e15; color: #e2e8f0; }
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
  <h2>Picker</h2>
  <p>Choose a browser mood:
    <select name="mood">
      <option>Fast</option>
      <option selected>Small</option>
      <option>Curious</option>
    </select>
  </p>
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
  <h2>Images</h2>
  <p>The PNG below is decoded from an inline <code>data:</code> URI:</p>
  <img alt="inline png" src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR42mP8z8Dwn4ECwESJ5lEDRgYAUf4CHnLwGuwAAAAASUVORK5CYII=">
  <p class="note">Numeric entities work too: &#65;&#66;&#67; and &#x2192; arrows.</p>
</body>
</html>
"#;
