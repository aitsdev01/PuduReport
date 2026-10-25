// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Conversor de markdown a markup de Typst.
//!
//! README.dev.md prefiere el paquete `cmarker` dentro del template, con fallback a
//! convertir en Rust con pulldown-cmark. Elegimos el fallback para garantizar
//! operacion 100% offline (sin descargar paquetes del registro de Typst), algo
//! clave para trabajo bajo NDA.
//!
//! La salida es markup de Typst que el template renderiza con
//! `eval(body, mode: "markup")`.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Profundidad maxima de anidamiento que se refleja en el markup de Typst
/// (sangria de listas, citas y negrita/cursiva anidadas). Mas alla, el
/// contenido se sigue emitiendo pero sin sumar sangria ni envoltorios: un
/// informe real no pasa de unos pocos niveles y, sin este tope, una lista
/// anidada de N niveles producia O(N^2) bytes de salida (ej. 32 KB de
/// `- - - ...` generaban 256 MB antes de llegar a Typst).
pub const MAX_NESTING: usize = 16;

/// Factor maximo entre la salida y la entrada de `to_typst`.
pub const OUTPUT_FACTOR: usize = 8;
/// Tope minimo de salida: un texto corto nunca se rechaza por el factor.
pub const MIN_OUTPUT_BYTES: usize = 1024 * 1024;
/// Tope absoluto de salida de una sola conversion.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Error de conversion de markdown.
#[derive(Debug, thiserror::Error)]
pub enum MarkdownError {
    #[error(
        "El contenido de un hallazgo o seccion genera demasiado markup para el PDF (mas de {limit} bytes a partir de {input} bytes de texto). Revisa listas, citas, tablas, enlaces o imagenes repetidos o anidados en exceso."
    )]
    OutputTooLarge { input: usize, limit: usize },
}

/// Tope de salida para una entrada de `input_len` bytes:
/// `max(8 x entrada, 1 MiB)`, nunca mas de 64 MiB.
pub fn output_limit(input_len: usize) -> usize {
    input_len
        .saturating_mul(OUTPUT_FACTOR)
        .clamp(MIN_OUTPUT_BYTES, MAX_OUTPUT_BYTES)
}

/// Estado de anidamiento durante la conversion.
#[derive(Default)]
struct Nesting {
    /// Una entrada por lista abierta (numero del proximo item si es ordenada).
    lists: Vec<Option<u64>>,
    /// Citas abiertas (incluidas las que superan `MAX_NESTING`).
    quotes: usize,
    /// Negritas/cursivas abiertas (incluidas las que superan `MAX_NESTING`).
    inline: usize,
}

/// Convierte markdown a una cadena de markup de Typst.
///
/// Falla si la salida supera `output_limit(markdown.len())`. El tope se
/// controla despues de cada evento, asi que la memoria usada queda acotada
/// aunque la entrada este disenada para amplificarse (por ejemplo, una
/// referencia de enlace con una URL larga usada miles de veces o una tabla con
/// muchas columnas y filas casi vacias, que pulldown-cmark completa con celdas).
pub fn to_typst(markdown: &str) -> Result<String, MarkdownError> {
    let limit = output_limit(markdown.len());
    let parser = Parser::new_ext(markdown, Options::ENABLE_TABLES);
    let mut out = String::new();
    let mut nesting = Nesting::default();
    // Buffer del bloque de codigo: (lenguaje, contenido). El fence se calcula al
    // cerrar (ver code_block_typst) para que ninguna racha de backticks del
    // contenido cierre el raw de Typst antes de tiempo (evita inyeccion de markup).
    let mut code_block: Option<(String, String)> = None;
    // Para imagenes: capturamos url + alt (que puede llevar el ancho, ej "60%")
    // y emitimos #image(..) al cerrar.
    let mut image_url: Option<String> = None;
    let mut image_alt = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                image_url = Some(dest_url.to_string());
                image_alt.clear();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code_block = Some((lang, String::new()));
            }
            Event::Start(tag) => start_tag(&mut out, &mut nesting, tag),
            Event::End(TagEnd::Image) => {
                if let Some(url) = image_url.take() {
                    out.push_str(&image_typst(&url, &image_alt));
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, content)) = code_block.take() {
                    out.push_str(&code_block_typst(&lang, &content));
                }
            }
            Event::End(tag) => end_tag(&mut out, &mut nesting, tag),
            Event::Text(text) => {
                if let Some((_, content)) = code_block.as_mut() {
                    // Dentro de un bloque de codigo el texto va literal (se
                    // encierra en un raw con fence suficientemente largo al cerrar).
                    content.push_str(&text);
                } else if image_url.is_some() {
                    // Texto dentro de una imagen: es el alt (lleva el ancho).
                    image_alt.push_str(&text);
                } else {
                    out.push_str(&escape(&text));
                }
            }
            Event::Code(code) => {
                out.push('`');
                out.push_str(&soft_break_long_runs(&code.replace('`', "\u{2018}")));
                out.push('`');
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push_str(" \\\n"),
            Event::Rule => out.push_str("\n#line(length: 100%)\n\n"),
            // HTML embebido se ignora para evitar inyectar markup arbitrario.
            Event::Html(_) | Event::InlineHtml(_) => {}
            _ => {}
        }
        // Ningun evento agrega mas de unas pocas veces su propio largo (que ya
        // esta en la entrada), asi que controlar aca acota la memoria.
        if out.len() > limit {
            return Err(MarkdownError::OutputTooLarge {
                input: markdown.len(),
                limit,
            });
        }
    }
    Ok(out.trim().to_string())
}

fn start_tag(out: &mut String, nesting: &mut Nesting, tag: Tag) {
    match tag {
        Tag::Heading { level, .. } => {
            let depth = heading_depth(level);
            out.push_str(&"=".repeat(depth));
            out.push(' ');
        }
        Tag::Table(alignments) => {
            let align = alignments
                .iter()
                .map(|a| match a {
                    pulldown_cmark::Alignment::Center => "center",
                    pulldown_cmark::Alignment::Right => "right",
                    _ => "left",
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "\n#table(columns: {}, align: ({align},), inset: 6pt, stroke: 0.5pt + luma(180),\n",
                alignments.len()
            ));
        }
        Tag::TableHead => out.push_str("table.header("),
        Tag::TableCell => out.push('['),
        Tag::Paragraph => {}
        // Se usa la funcion explicita en vez del shorthand `*.../_...`: el
        // shorthand tiene reglas de flanking propias de Typst que no siempre
        // coinciden con los limites que decidio pulldown-cmark, y un vecino
        // escapado puede dejar el delimitador "sin cerrar" para Typst.
        // Pasado `MAX_NESTING` el texto sigue, sin otro envoltorio.
        Tag::Strong | Tag::Emphasis => {
            nesting.inline += 1;
            if nesting.inline <= MAX_NESTING {
                out.push_str(if matches!(tag, Tag::Strong) {
                    "#strong["
                } else {
                    "#emph["
                });
            }
        }
        Tag::List(start) => {
            // Una lista anidada en un item ajustado (sin parrafo) llega justo
            // despues del texto del item padre: sin el salto de linea, Typst
            // leeria `- a  - b` como un solo item.
            if !nesting.lists.is_empty() && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            nesting.lists.push(start);
        }
        Tag::Item => {
            // Sangria acotada: los niveles mas profundos que `MAX_NESTING`
            // quedan alineados con el ultimo nivel permitido.
            let depth = nesting.lists.len().saturating_sub(1).min(MAX_NESTING);
            out.push_str(&"  ".repeat(depth));
            match nesting.lists.last_mut() {
                Some(Some(n)) => {
                    out.push_str(&format!("{n}. "));
                    *n += 1;
                }
                _ => out.push_str("- "),
            }
        }
        // Los bloques de codigo se bufferean en el bucle principal (el fence se
        // calcula al cerrar para que el contenido no pueda romper el raw).
        Tag::Link { dest_url, .. } => {
            out.push_str(&format!("#link(\"{}\")[", escape_string(&dest_url)));
        }
        // Las imagenes se manejan en el bucle principal (necesitan el alt/ancho).
        // Pasado `MAX_NESTING` el contenido de la cita sigue, sin otro marco.
        Tag::BlockQuote(_) => {
            nesting.quotes += 1;
            if nesting.quotes <= MAX_NESTING {
                out.push_str("#quote(block: true)[\n");
            }
        }
        _ => {}
    }
}

fn end_tag(out: &mut String, nesting: &mut Nesting, tag: TagEnd) {
    match tag {
        TagEnd::TableCell => out.push_str("],\n"),
        TagEnd::TableHead => out.push_str("),\n"),
        TagEnd::Table => out.push_str(")\n\n"),
        TagEnd::Heading(_) => out.push_str("\n\n"),
        TagEnd::Paragraph => out.push_str("\n\n"),
        TagEnd::Strong | TagEnd::Emphasis => {
            if nesting.inline <= MAX_NESTING {
                out.push(']');
            }
            nesting.inline = nesting.inline.saturating_sub(1);
        }
        TagEnd::List(_) => {
            nesting.lists.pop();
            out.push('\n');
        }
        TagEnd::Item => out.push('\n'),
        TagEnd::Link => out.push(']'),
        TagEnd::BlockQuote(_) => {
            if nesting.quotes <= MAX_NESTING {
                out.push_str("\n]\n\n");
            }
            nesting.quotes = nesting.quotes.saturating_sub(1);
        }
        _ => {}
    }
}

fn heading_depth(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Escapa caracteres con significado especial en markup de Typst.
///
/// Incluye `/` porque en Typst `//` y `/* */` son comentarios: sin escapar, la
/// prosa con `//` (o un `http://` suelto) desapareceria del PDF.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '>' | '@' | '[' | ']' | '/' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Longitud de la racha mas larga de backticks consecutivos en `s`.
fn longest_backtick_run(s: &str) -> usize {
    let mut max = 0;
    let mut cur = 0;
    for ch in s.chars() {
        if ch == '`' {
            cur += 1;
            max = max.max(cur);
        } else {
            cur = 0;
        }
    }
    max
}

/// Emite un bloque de codigo raw de Typst. El fence usa mas backticks que
/// cualquier racha del contenido (Typst lo permite), de modo que el contenido
/// no pueda cerrar el raw antes de tiempo e inyectar markup/codigo.
fn code_block_typst(lang: &str, content: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(content).max(2) + 1);
    // El contenido suele venir con un salto final; se normaliza a uno solo.
    let body = content.strip_suffix('\n').unwrap_or(content);
    let body = soft_break_long_runs(body);
    format!("{fence}{lang}\n{body}\n{fence}\n\n")
}

/// Inserta oportunidades de corte (espacio de ancho cero, U+200B) dentro de
/// rachas largas de caracteres sin espacios —tokens tipo JWT, hash o base64—
/// para que Typst pueda quebrarlas y el bloque de codigo no se salga del ancho
/// de la pagina. Solo afecta rachas de mas de `MAX_RUN` caracteres, asi que los
/// identificadores y palabras clave cortas quedan intactos (no rompe el
/// resaltado de sintaxis). El ZWSP es invisible en el PDF.
fn soft_break_long_runs(s: &str) -> String {
    const MAX_RUN: usize = 42;
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for ch in s.chars() {
        out.push(ch);
        if ch.is_whitespace() {
            run = 0;
        } else {
            run += 1;
            if run >= MAX_RUN {
                out.push('\u{200B}');
                run = 0;
            }
        }
    }
    out
}

/// Escapa una cadena para usar dentro de comillas dobles de Typst.
fn escape_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Construye la llamada #image centrada, aplicando el ancho si el alt lo indica.
///
/// Si `url` no es una ruta de asset del proyecto (ver `is_asset_path`), no se
/// emite `#image(..)`: el usuario puede haber pegado markdown con una imagen
/// referenciando un archivo de su disco (ruta absoluta de Windows, con `:` o
/// `\`), y Typst no puede cargarla (sale del `--root` del proyecto) ni sabe
/// interpretar backslashes como separador, lo que rompe toda la exportacion
/// con un error de sistema operativo crudo. Se muestra un aviso en su lugar.
fn image_typst(url: &str, alt: &str) -> String {
    if !is_asset_path(url) {
        return format!(
            "#align(center, block(fill: luma(245), inset: 8pt, radius: 3pt)[#text(size: 8pt, style: \"italic\", fill: luma(120))[Imagen no disponible: {}]])",
            escape(url)
        );
    }
    let img = match parse_width(alt) {
        Some(width) => format!("image(\"{}\", width: {})", escape_string(url), width),
        None => format!("image(\"{}\")", escape_string(url)),
    };
    // Las evidencias van centradas.
    format!("#align(center, {img})")
}

/// Es una ruta de asset valida del proyecto (la unica forma en que la app
/// inserta imagenes, ver `workspace::save_asset`): relativa, bajo `assets/`,
/// sin separador de Windows ni forma de escapar el `--root` del proyecto.
fn is_asset_path(url: &str) -> bool {
    url.starts_with("assets/") && !url.contains("..") && !url.contains('\\') && !url.contains(':')
}

/// Interpreta el alt como un ancho ("60%" o "60") y devuelve el ancho Typst.
fn parse_width(alt: &str) -> Option<String> {
    let trimmed = alt.trim();
    let digits = trimmed.strip_suffix('%').unwrap_or(trimmed);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let pct: u32 = digits.parse().ok()?;
    if (1..=100).contains(&pct) {
        Some(format!("{pct}%"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Conversion que no debe fallar en estas pruebas.
    fn typ(markdown: &str) -> String {
        to_typst(markdown).unwrap()
    }

    /// Profundidad de las pruebas de anidamiento extremo.
    const DEEP: usize = 50_000;
    /// Tiempo maximo de una conversion extrema (holgado para modo debug y CI).
    const DEEP_MAX_TIME: Duration = Duration::from_secs(10);

    /// Convierte `input` y verifica que termine a tiempo y que, si no falla
    /// por el tope, la salida no lo supere. Devuelve el resultado.
    fn convert_bounded(input: &str) -> Result<String, MarkdownError> {
        let started = Instant::now();
        let result = to_typst(input);
        assert!(started.elapsed() < DEEP_MAX_TIME, "{:?}", started.elapsed());
        match &result {
            Ok(out) => assert!(out.len() <= output_limit(input.len()), "{}", out.len()),
            Err(MarkdownError::OutputTooLarge { input: n, limit }) => {
                assert_eq!(*n, input.len());
                assert_eq!(*limit, output_limit(input.len()));
            }
        }
        result
    }

    /// Mayor sangria (en espacios) de las lineas de `out`.
    fn max_indent(out: &str) -> usize {
        out.lines()
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn deeply_nested_list_indent_is_clamped() {
        // Antes: 32 KB de `- - - ...` generaban 256 MB (sangria cuadratica).
        // Con la sangria acotada la salida crece en forma lineal.
        let depth = 20_000;
        let input = format!("{}x", "- ".repeat(depth));
        let out = convert_bounded(&input).unwrap();
        assert_eq!(max_indent(&out), 2 * MAX_NESTING);
        assert!(out.len() <= depth * (2 * MAX_NESTING + 8), "{}", out.len());
        assert!(out.ends_with("- x"), "{}", &out[out.len() - 80..]);
    }

    #[test]
    fn list_nested_50k_levels_is_bounded() {
        for marker in ["- ", "1. "] {
            let input = format!("{}x", marker.repeat(DEEP));
            if let Ok(out) = convert_bounded(&input) {
                assert_eq!(max_indent(&out), 2 * MAX_NESTING);
            }
        }
    }

    #[test]
    fn deeply_nested_blockquotes_are_bounded_and_balanced() {
        let input = format!("{}x", ">".repeat(DEEP));
        let out = convert_bounded(&input).unwrap();
        assert!(out.len() <= input.len() * OUTPUT_FACTOR, "{}", out.len());
        assert_eq!(out.matches("#quote(block: true)[").count(), MAX_NESTING);
        assert_eq!(out.matches('[').count(), out.matches(']').count());
        assert!(out.contains('x'));
    }

    #[test]
    fn deeply_nested_emphasis_is_bounded_and_balanced() {
        let input = format!("{}x{}", "*".repeat(DEEP), "*".repeat(DEEP));
        let out = convert_bounded(&input).unwrap();
        assert!(out.len() <= input.len() * OUTPUT_FACTOR, "{}", out.len());
        let wrappers = out.matches("#strong[").count() + out.matches("#emph[").count();
        assert!(wrappers <= MAX_NESTING, "{wrappers}");
        assert_eq!(out.matches('[').count(), out.matches(']').count());
    }

    #[test]
    fn moderate_nesting_is_preserved() {
        let out = typ("- a\n  - b\n    - c");
        assert!(out.contains("- a"), "{out}");
        assert!(out.contains("\n  - b"), "{out}");
        assert!(out.contains("\n    - c"), "{out}");
        let quotes = typ("> > > cita");
        assert_eq!(quotes.matches("#quote(block: true)[").count(), 3);
        assert!(typ("***a***").contains("#emph[#strong[a]]"));
    }

    #[test]
    fn amplified_link_references_are_bounded() {
        // Una referencia con URL larga usada muchas veces: la salida queda
        // acotada (por pulldown-cmark o por el tope de `to_typst`).
        let url = format!("https://x.example/{}", "a".repeat(30_000));
        let input = format!("[r]: {url}\n\n{}", "[r] ".repeat(30_000));
        let _ = convert_bounded(&input);
    }

    #[test]
    fn table_padding_amplification_is_rejected() {
        // pulldown-cmark completa las filas cortas con celdas vacias: muchas
        // columnas y muchas filas de un caracter crecen como columnas x filas.
        let cols = 5_000;
        let input = format!(
            "{}|\n{}|\n{}",
            "|a".repeat(cols),
            "|-".repeat(cols),
            "x\n".repeat(5_000)
        );
        let err = convert_bounded(&input).unwrap_err();
        assert!(err.to_string().contains("demasiado markup"), "{err}");
    }

    #[test]
    fn many_broken_images_are_bounded() {
        let input = "![](x) ".repeat(DEEP);
        let _ = convert_bounded(&input);
    }

    #[test]
    fn output_limit_has_floor_and_hard_cap() {
        assert_eq!(output_limit(0), MIN_OUTPUT_BYTES);
        assert_eq!(output_limit(1024 * 1024), 8 * 1024 * 1024);
        assert_eq!(output_limit(usize::MAX), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn gfm_table_preserves_cells_alignment_and_escaped_content() {
        let out = typ("| IP | Host | Nota |\n|:---|:---:|---:|\n| `192.0.2.1` | **router** | #literal [x] |\n| 192.0.2.2 | | fin |");
        assert!(
            out.contains("#table(columns: 3, align: (left, center, right,)"),
            "{out}"
        );
        assert!(out.contains("table.header([IP]"), "{out}");
        assert!(out.contains("[#strong[router]]"), "{out}");
        assert!(out.contains("[\\#literal \\[x\\]]"), "{out}");
        assert!(out.contains("[],"), "{out}");
        assert!(!out.contains("|---"));
    }

    #[test]
    fn heading_and_paragraph() {
        let out = typ("## Descripcion\n\nUn texto.");
        assert!(out.contains("== Descripcion"));
        assert!(out.contains("Un texto."));
    }

    #[test]
    fn emphasis_and_strong() {
        let out = typ("Esto es **fuerte** y *enfasis*.");
        assert!(out.contains("#strong[fuerte]"));
        assert!(out.contains("#emph[enfasis]"));
    }

    #[test]
    fn emphasis_followed_by_letter_stays_balanced() {
        // Reproducido contra el binario de Typst: con el shorthand previo
        // (`*a*mas` o `_b_mas`, con la palabra pegada justo despues del
        // cierre) Typst reporta "error: unclosed delimiter" en el eval() del
        // hallazgo, porque sus reglas de flanking no reconocen ese cierre
        // como valido (le sigue una letra sin espacio) y nunca encuentra un
        // cierre "aceptable" para la apertura. Esto rompia la exportacion
        // completa del PDF con contenido de aspecto normal (ver caso real de
        // un hallazgo cuyo cuerpo tenia una palabra en cursiva pegada al
        // texto siguiente). #strong[..]/#emph[..] no dependen de flanking,
        // asi que siempre quedan balanceados.
        let out = typ("*a*mas y tambien _b_mas");
        assert!(out.contains("#emph[a]mas"));
        assert!(out.contains("\\_b\\_mas"), "out: {out}");
    }

    #[test]
    fn code_block() {
        let out = typ("```sql\nSELECT 1\n```");
        assert!(out.contains("```sql"));
        assert!(out.contains("SELECT 1"));
    }

    #[test]
    fn code_block_content_is_not_escaped() {
        // Dentro de un bloque raw los caracteres especiales van literales.
        let out = typ("```\npassword: cualquier_cosa <token>\n```");
        assert!(out.contains("cualquier_cosa <token>"));
        assert!(!out.contains("cualquier\\_cosa"));
    }

    #[test]
    fn long_token_gets_soft_break_opportunities() {
        // Un token largo sin espacios (JWT/hash) recibe ZWSP para poder
        // quebrarse y no salirse del ancho del bloque en el PDF.
        let token = "a".repeat(120);
        let out = typ(&format!("```\n{token}\n```"));
        assert!(out.contains('\u{200B}'), "deberia insertar ZWSP: {out}");
    }

    #[test]
    fn short_code_is_left_intact() {
        // Codigo corto (keywords, identificadores) no debe recibir ZWSP, para
        // no romper el resaltado de sintaxis.
        let out = typ("```\nSELECT * FROM users WHERE id = 1\n```");
        assert!(!out.contains('\u{200B}'), "no deberia insertar ZWSP: {out}");
        let inline = typ("El valor `admin` es sensible.");
        assert!(!inline.contains('\u{200B}'));
    }

    #[test]
    fn escapes_special_chars() {
        // Un # crudo no debe romper el markup de Typst.
        let out = typ("precio #1 con $5");
        assert!(out.contains("\\#1"));
        assert!(out.contains("\\$5"));
    }

    #[test]
    fn escapes_slash_to_avoid_typst_comments() {
        // En Typst `//` es comentario: sin escapar, "b" desapareceria del PDF.
        let out = typ("ver http://acme.com y a//b");
        assert!(out.contains("http:\\/\\/acme.com"));
        assert!(out.contains("a\\/\\/b"));
    }

    #[test]
    fn code_block_fence_outgrows_content_backticks() {
        // Un bloque cuyo contenido trae ``` (fence markdown de 4) no debe poder
        // cerrar el raw de Typst antes de tiempo: el fence emitido es mas largo.
        let out = typ("````\n```\n#sys\n````");
        // Todo el bloque queda envuelto en un fence de 4 backticks; el ``` y el
        // #sys del contenido quedan literales adentro, no como markup de Typst.
        assert!(
            out.starts_with("````"),
            "fence debe ser de 4+ backticks: {out}"
        );
        assert!(
            out.ends_with("````"),
            "debe cerrar con el mismo fence: {out}"
        );
        assert!(out.contains("```\n#sys"), "el ``` interno queda literal");
    }

    #[test]
    fn link_conversion() {
        let out = typ("[ACME](https://acme.com)");
        assert!(out.contains("#link(\"https://acme.com\")[ACME]"));
    }

    #[test]
    fn image_conversion() {
        // El alt no debe quedar como texto suelto despues de la imagen.
        let out = typ("![captura de evidencia](assets/ab12.png)");
        assert!(out.contains("image(\"assets/ab12.png\")"));
        assert!(out.contains("align(center"));
        assert!(!out.contains("captura de evidencia"));
        assert!(!out.contains("width:"));
    }

    #[test]
    fn image_with_windows_absolute_path_is_not_emitted_as_image() {
        // Caso real reportado: el usuario pego markdown con una imagen
        // apuntando a un archivo de su disco (ruta absoluta de Windows). Sin
        // esta validacion, #image(..) con esa ruta rompe toda la exportacion
        // (Typst no puede resolver `\` ni salir del --root del proyecto).
        let out = typ("![captura](C:\\Users\\Cristobal.aguero\\Desktop\\captura.png)");
        assert!(!out.contains("#image"), "out: {out}");
        assert!(out.contains("Imagen no disponible"));
    }

    #[test]
    fn image_path_traversal_is_not_emitted_as_image() {
        let out = typ("![x](assets/../../../etc/passwd)");
        assert!(!out.contains("#image"), "out: {out}");
    }

    #[test]
    fn image_width_from_alt() {
        let out = typ("![60%](assets/ab12.png)");
        assert!(out.contains("image(\"assets/ab12.png\", width: 60%)"));

        // Sin sufijo % tambien se interpreta como porcentaje.
        let out2 = typ("![35](assets/x.png)");
        assert!(out2.contains("width: 35%"));

        // Valor fuera de rango se ignora.
        let out3 = typ("![999](assets/x.png)");
        assert!(!out3.contains("width:"));
    }
}
