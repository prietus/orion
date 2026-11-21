use orion_html::{Color, Doc, LayoutOptions};

fn render(html: &str, width: u16) -> String {
    let doc = Doc::parse(html);
    doc.layout(width, &LayoutOptions::default()).text()
}

#[test]
fn paragraphs_headings_inline() {
    let t = render("<h1>Título</h1><p>Hola <b>mundo</b>, esto   es\n un <i>párrafo</i> largo que se ajusta.</p><p>Segundo&nbsp;&nbsp;párrafo</p>", 20);
    assert_eq!(t, "Título\n\nHola mundo, esto es\nun párrafo largo que\nse ajusta.\n\nSegundo  párrafo");
}

#[test]
fn hidden_preheader_and_style_block() {
    let html = r##"<html><head><style>
        .hide { display:none } p.note { color: #ff0000 } #main p.note { color: #00ff00 }
        @media (max-width: 600px) { .note { display:none } }
      </style></head><body>
      <div class="hide">Texto de previsualización oculto</div>
      <span style="font-size:0;max-height:0;overflow:hidden">otro oculto</span>
      <div style="display:none !important">tampoco</div>
      <div id="main"><p class="note">verde por especificidad</p></div>
      <p class="note" style="color:blue">inline gana</p>
    </body></html>"##;
    let doc = Doc::parse(html);
    let r = doc.layout(60, &LayoutOptions::default());
    assert_eq!(r.text(), "verde por especificidad\n\ninline gana");
    let colors: Vec<_> = r.lines.iter().filter_map(|l| l.spans.first().and_then(|s| s.style.fg)).collect();
    assert_eq!(colors, [Color::Rgb(0, 255, 0), Color::Rgb(0, 0, 255)]);
}

#[test]
fn links_are_numbered() {
    let doc = Doc::parse(r##"<p>Ver <a href="https://example.com/a">la web</a> o <a href="#ancla">ancla</a> o <a href="mailto:x@y.z">escribir</a>.</p>"##);
    assert_eq!(doc.links, ["https://example.com/a", "mailto:x@y.z"]);
    let r = doc.layout(80, &LayoutOptions::default());
    assert_eq!(r.text(), "Ver la web[1] o ancla o escribir[2].");
    let link_span = r.lines[0].spans.iter().find(|s| s.text == "la web").unwrap();
    assert_eq!(link_span.style.link, Some(0));
    assert!(link_span.style.underline);
}

#[test]
fn lists_and_quotes() {
    let t = render("<ul><li>uno</li><li>dos<ul><li>anidado</li></ul></li></ul><ol start=9><li>nueve</li><li>diez</li></ol><blockquote>cita<blockquote>doble</blockquote></blockquote>", 40);
    assert_eq!(t, "• uno\n• dos\n  ◦ anidado\n\n 9. nueve\n10. diez\n\n│ cita\n│\n│ │ doble");
}

#[test]
fn pre_keeps_spacing() {
    let t = render("<pre>fn main() {\n    println!(\"hola\");\n}</pre>", 40);
    assert_eq!(t, "fn main() {\n    println!(\"hola\");\n}");
}

#[test]
fn data_table_with_border() {
    let t = render(
        "<table border=1><tr><th>Producto</th><th>Precio</th></tr><tr><td>Café</td><td align=right>1,20 €</td></tr>\
         <tr><td>Tostada con tomate</td><td>2,50 €</td></tr></table>",
        60,
    );
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines[0].trim_end(), "     Producto      │ Precio");
    assert_eq!(lines[1], "─".repeat(27), "the rule is as wide as the table");
    assert_eq!(lines[2].trim_end(), "Café               │ 1,20 €");
    assert_eq!(lines[3].trim_end(), "Tostada con tomate │ 2,50 €");
}

#[test]
fn layout_tables_newsletter() {
    // Typical newsletter: centred 600px table, spacer columns, nested tables.
    let html = r##"
    <table width="100%" bgcolor="#eeeeee"><tr><td align="center">
      <table width="600"><tr><td>
        <table><tr>
          <td width="20"></td>
          <td><h2>Noticia uno</h2><p>Resumen de la primera noticia con bastante texto para que haya que ajustarlo.</p></td>
          <td width="20"></td>
          <td><h2>Noticia dos</h2><p>Otro resumen.</p></td>
        </tr></table>
      </td></tr></table>
    </td></tr></table>"##;
    let t = render(html, 50);
    println!("{t}");
    let lines: Vec<&str> = t.lines().collect();
    assert!(lines[0].contains("Noticia uno") && lines[0].contains("Noticia dos"), "columns side by side: {lines:?}");
    assert!(lines.iter().all(|l| l.chars().count() <= 50), "nothing exceeds the width");
    assert!(!lines[0].starts_with(' '), "empty spacer columns take no space");
}

#[test]
fn narrow_width_never_overflows() {
    let html = "<table><tr><td>palabraextremadamentelargaquenocabe</td><td>b</td><td>c</td></tr></table><p>https://example.com/una/url/muy/larga/que/no/cabe/en/la/linea</p>";
    for w in [10u16, 15, 25] {
        let doc = Doc::parse(html);
        let r = doc.layout(w, &LayoutOptions::default());
        for l in &r.lines {
            assert!(l.width() <= w as usize, "width {w}: {:?} is {} wide", l.text(), l.width());
        }
    }
}

#[test]
fn cid_images_are_placed() {
    let doc = Doc::parse(r##"<p>Antes</p><img src="cid:logo@x" width="200"><img src="https://tracker/p.gif" width="1" height="1"><img src="https://remota/foto.jpg" alt="Foto remota"><p>Después</p>"##);
    assert_eq!(doc.images.len(), 2, "the tracking pixel is dropped");
    assert_eq!(doc.images[0].content_id(), Some("logo@x"));
    let px = |i: usize| (i == 0).then_some((400u32, 200u32));
    let opts = LayoutOptions { cell: (10, 20), max_image_rows: 40, link_markers: true, image_px: &px };
    let r = doc.layout(80, &opts);
    // 200px wide (attribute) → 100px high → 20 columns x 5 rows.
    assert_eq!(r.placements.len(), 1);
    let p = r.placements[0];
    assert_eq!((p.width, p.height, p.x), (20, 5, 0));
    assert_eq!(r.lines[p.line - 1].text(), "", "margin after the paragraph");
    let t = r.text();
    assert!(t.contains("[Foto remota]") && t.ends_with("Después"));

    // Image wider than the column: scaled down keeping the aspect ratio.
    let r = doc.layout(10, &opts);
    assert_eq!((r.placements[0].width, r.placements[0].height), (10, 3));
}

#[test]
fn plain_text_quotes_and_urls() {
    let doc = Doc::from_plain("Hola,\n> cita con https://example.com/x.\n>> más\n\n  sangrado");
    assert_eq!(doc.links, ["https://example.com/x"]);
    let r = doc.layout(80, &LayoutOptions::default());
    assert_eq!(r.text(), "Hola,\n> cita con https://example.com/x[1].\n>> más\n\n  sangrado");
    assert_eq!(r.lines[1].spans[0].style.fg, Some(Color::Ansi(2)));
}

#[test]
fn malformed_html_does_not_panic() {
    for html in ["<table><td>sin tr", "<p><div>bloque en p</p></div>", "<ul><li>a<li>b", "<<<>>>", "", "<a href=https://x.y>enlace<div>con bloque</div></a>"] {
        let doc = Doc::parse(html);
        let _ = doc.layout(30, &LayoutOptions::default());
    }
}
