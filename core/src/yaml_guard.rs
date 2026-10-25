// SPDX-License-Identifier: GPL-3.0-only
// Copyright (c) 2026 bc0d3

//! Revision previa y barata del YAML de un paquete, antes de `serde_yaml`.
//!
//! `serde_yaml` no acota el trabajo por nodos y expande anclas y alias. Esta
//! pasada acota anidamiento y nodos con las mismas reglas del lector de PuduHub
//! (`informes/documento.py`, `_Lector` y `PresupuestoYaml`) y rechaza lo que
//! ese lector rechaza: anclas, alias y etiquetas en posicion de nodo,
//! directivas y varios documentos.
//!
//! Cuenta como nodo cada entrada de un mapa de bloque, cada elemento de una
//! secuencia de bloque y cada nodo de una coleccion de flujo (la coleccion,
//! sus claves y sus valores). La profundidad cuenta las colecciones abiertas,
//! de bloque y de flujo juntas, como el limite de valores de PuduHub
//! (`resolver`).

/// Profundidad maxima de colecciones anidadas.
pub(crate) const MAX_DEPTH: usize = 32;
/// Nodos maximos de un archivo YAML.
pub(crate) const MAX_YAML_NODES: u64 = 50_000;
/// Nodos y caracteres maximos de todos los YAML de un documento
/// (`project.yaml` y todos los front-matter).
pub(crate) const MAX_DOCUMENT_YAML_NODES: u64 = 500_000;
pub(crate) const MAX_DOCUMENT_YAML_CHARS: u64 = 8 * 1024 * 1024;

type Result<T> = std::result::Result<T, String>;

/// Nodos y caracteres que quedan para los YAML de un mismo documento.
pub(crate) struct YamlBudget {
    nodes: u64,
    chars: u64,
}

impl Default for YamlBudget {
    fn default() -> Self {
        YamlBudget {
            nodes: MAX_DOCUMENT_YAML_NODES,
            chars: MAX_DOCUMENT_YAML_CHARS,
        }
    }
}

impl YamlBudget {
    /// Revisa un archivo YAML y descuenta su trabajo del presupuesto.
    pub(crate) fn check(&mut self, text: &str) -> Result<()> {
        let chars = text.chars().count() as u64;
        if chars > self.chars {
            return Err("el YAML del documento es demasiado grande".to_string());
        }
        self.chars -= chars;
        let mut scanner = Scanner::new(text, MAX_YAML_NODES.min(self.nodes));
        scanner.run()?;
        self.nodes -= scanner.nodes;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Map,
    Seq,
}

struct Scanner<'a> {
    text: &'a [u8],
    /// Inicio (byte) de cada linea; la ultima entrada es el largo del texto.
    starts: Vec<usize>,
    nodes: u64,
    max_nodes: u64,
    /// Colecciones de bloque abiertas: (sangria, tipo).
    stack: Vec<(usize, Kind)>,
}

fn error(line: usize, message: &str) -> String {
    format!("YAML linea {}: {message}", line + 1)
}

fn blank(line: &[u8]) -> bool {
    let trimmed = trim_start(line);
    trimmed.is_empty() || trimmed[0] == b'#'
}

fn trim_start(bytes: &[u8]) -> &[u8] {
    let skip = bytes
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count();
    &bytes[skip..]
}

fn marker(line: &[u8], marker: &[u8]) -> bool {
    line.starts_with(marker) && (line.len() == 3 || matches!(line[3], b' ' | b'\t'))
}

/// `-`, `?` o `:` seguido de espacio o fin de linea.
fn block_indicator(content: &[u8], indicator: u8) -> bool {
    content.first() == Some(&indicator)
        && (content.len() == 1 || matches!(content[1], b' ' | b'\t'))
}

const INDICATORS: &[u8] = b"-?:,[]{}#&*!|>'\"%@`";
const FLOW: &[u8] = b",[]{}";

impl<'a> Scanner<'a> {
    fn new(text: &'a str, max_nodes: u64) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes();
        let mut starts = vec![0];
        for (index, &b) in text.iter().enumerate() {
            if b == b'\n' {
                starts.push(index + 1);
            }
        }
        starts.push(text.len() + 1);
        Scanner {
            text,
            starts,
            nodes: 0,
            max_nodes,
            stack: Vec::new(),
        }
    }

    fn line_count(&self) -> usize {
        self.starts.len() - 1
    }

    /// Linea `index` sin el salto final (ni `\r`).
    fn line(&self, index: usize) -> &'a [u8] {
        let start = self.starts[index].min(self.text.len());
        let end = (self.starts[index + 1] - 1).min(self.text.len());
        let line = &self.text[start..end];
        line.strip_suffix(b"\r").unwrap_or(line)
    }

    fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index - 1,
        }
    }

    fn count(&mut self, line: usize) -> Result<()> {
        self.nodes += 1;
        if self.nodes > self.max_nodes {
            return Err(error(line, "demasiados nodos"));
        }
        Ok(())
    }

    fn enter(&mut self, line: usize, indent: usize, kind: Kind, indentless: bool) -> Result<()> {
        while let Some(&(top, top_kind)) = self.stack.last() {
            // Secuencia sin sangria como valor de una clave ("clave:\n- a").
            if indentless && top == indent && top_kind == Kind::Map && kind == Kind::Seq {
                break;
            }
            if top > indent || (top == indent && top_kind != kind) {
                self.stack.pop();
            } else {
                break;
            }
        }
        if self.stack.last() != Some(&(indent, kind)) {
            self.stack.push((indent, kind));
            if self.stack.len() > MAX_DEPTH {
                return Err(error(line, "anidamiento demasiado profundo"));
            }
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        let lines = self.line_count();
        let mut started = false;
        let mut seen_content = false;
        let mut index = 0;
        // Sangria del dueno de un valor pendiente (clave o guion sin valor en
        // su linea) y si admite una secuencia sin sangria.
        let mut pending: Option<(usize, bool)> = None;
        // Sangria del dueno de un escalar de bloque (`|` o `>`) en curso.
        let mut block_scalar: Option<usize> = None;
        while index < lines {
            let line = self.line(index);
            let indent = line.iter().take_while(|&&b| b == b' ').count();
            if let Some(parent) = block_scalar {
                if trim_start(line).is_empty() || indent > parent {
                    index += 1;
                    continue;
                }
                block_scalar = None;
            }
            if line.first() == Some(&b'%') && !started {
                return Err(error(index, "directivas no soportadas"));
            }
            if marker(line, b"---") {
                if started {
                    return Err(error(index, "varios documentos no soportados"));
                }
                if !blank(&line[3..]) {
                    return Err(error(index, "contenido despues de '---' no soportado"));
                }
                started = true;
                index += 1;
                continue;
            }
            if marker(line, b"...") {
                let rest_blank = (index + 1..lines).all(|i| blank(self.line(i)));
                if !blank(&line[3..]) || !rest_blank {
                    return Err(error(index, "varios documentos no soportados"));
                }
                return Ok(());
            }
            if blank(line) {
                index += 1;
                continue;
            }
            let node_position = match pending.take() {
                Some((owner, indentless)) => {
                    indent > owner || (indentless && indent == owner && line[indent] == b'-')
                }
                None => !seen_content,
            };
            started = true;
            seen_content = true;
            let indentless = node_position && line[indent] == b'-';
            let outcome = self.block_line(index, indent, node_position, indentless)?;
            match outcome {
                Outcome::Done => index += 1,
                Outcome::Pending(owner, indentless) => {
                    pending = Some((owner, indentless));
                    index += 1;
                }
                Outcome::BlockScalar(owner) => {
                    block_scalar = Some(owner);
                    index += 1;
                }
                Outcome::Resume(offset) => index = self.line_of(offset) + 1,
            }
        }
        Ok(())
    }

    /// Una linea de bloque desde `column`.
    fn block_line(
        &mut self,
        index: usize,
        indent: usize,
        mut node_position: bool,
        indentless: bool,
    ) -> Result<Outcome> {
        let line = self.line(index);
        let mut column = indent;
        let mut owner = indent;
        let mut first = true;
        loop {
            let content = &line[column..];
            let dash = block_indicator(content, b'-');
            let question = block_indicator(content, b'?');
            if dash || question {
                let kind = if dash { Kind::Seq } else { Kind::Map };
                self.enter(index, column, kind, first && indentless)?;
                self.count(index)?;
                let rest = &content[1..];
                let stripped = trim_start(rest);
                if stripped.is_empty() || stripped[0] == b'#' {
                    return Ok(Outcome::Pending(column, false));
                }
                owner = column;
                column += 1 + rest.len() - stripped.len();
                node_position = true;
                first = false;
                continue;
            }
            if block_indicator(content, b':') {
                // Valor de una entrada explicita ("? clave" / ": valor").
                let rest = &content[1..];
                let stripped = trim_start(rest);
                if stripped.is_empty() || stripped[0] == b'#' {
                    return Ok(Outcome::Pending(column, false));
                }
                owner = column;
                column += 1 + rest.len() - stripped.len();
                node_position = true;
                first = false;
                continue;
            }
            if let Some(after) = self.key(content, index)? {
                self.enter(index, column, Kind::Map, false)?;
                self.count(index)?;
                let rest = &content[after..];
                let stripped = trim_start(rest);
                if stripped.is_empty() || stripped[0] == b'#' {
                    return Ok(Outcome::Pending(column, true));
                }
                let value = column + after + rest.len() - stripped.len();
                return self.value(index, value, column);
            }
            if !node_position {
                // Continuacion de un escalar plano de varias lineas.
                return Ok(Outcome::Done);
            }
            return self.value(index, column, owner);
        }
    }

    /// `Some(desplazamiento tras ':')` si la linea es una entrada `clave: valor`.
    fn key(&self, content: &[u8], index: usize) -> Result<Option<usize>> {
        let Some(&first) = content.first() else {
            return Ok(None);
        };
        if first == b'\'' || first == b'"' {
            let Some(end) = quoted_end(content, 0) else {
                return Ok(None);
            };
            let rest = &content[end..];
            let stripped = trim_start(rest);
            if block_indicator(stripped, b':') {
                return Ok(Some(end + rest.len() - stripped.len() + 1));
            }
            return Ok(None);
        }
        let special = matches!(first, b'-' | b'?' | b':')
            && content.len() > 1
            && !matches!(content[1], b' ' | b'\t');
        if INDICATORS.contains(&first) && !special {
            return Ok(None);
        }
        for position in 0..content.len() {
            let b = content[position];
            if b == b'#' && position > 0 && matches!(content[position - 1], b' ' | b'\t') {
                return Ok(None);
            }
            if b == b':'
                && (position + 1 == content.len() || matches!(content[position + 1], b' ' | b'\t'))
            {
                let key = &content[..position];
                if key.len() > 1024 {
                    return Err(error(index, "clave demasiado larga"));
                }
                return Ok((!key.is_empty()).then_some(position + 1));
            }
        }
        Ok(None)
    }

    /// Nodo que empieza en `line[column]`; `owner` es la sangria de su dueno.
    fn value(&mut self, index: usize, column: usize, owner: usize) -> Result<Outcome> {
        let line = self.line(index);
        match line[column] {
            b'|' | b'>' => Ok(Outcome::BlockScalar(owner)),
            b'\'' | b'"' => {
                let start = self.starts[index] + column;
                let end = quoted_end(self.text, start).unwrap_or(self.text.len());
                Ok(Outcome::Resume(end.saturating_sub(1).max(start)))
            }
            b'[' | b'{' => {
                let start = self.starts[index] + column;
                let end = self.flow(start)?;
                Ok(Outcome::Resume(end.saturating_sub(1).max(start)))
            }
            b'&' | b'*' | b'!' => Err(error(index, "anclas, alias y etiquetas no soportados")),
            _ => Ok(Outcome::Done),
        }
    }

    /// Recorre una coleccion de flujo desde `start` (`[` o `{`); devuelve el
    /// byte siguiente a su cierre.
    fn flow(&mut self, start: usize) -> Result<usize> {
        let text = self.text;
        let mut offset = start;
        // Pila de colecciones abiertas: (es mapa, esperando valor).
        let mut open: Vec<(bool, bool)> = Vec::new();
        let mut expect_node = true;
        loop {
            offset = self.skip(offset);
            let Some(&b) = text.get(offset) else {
                // Sin cerrar: serde_yaml lo rechaza.
                return Ok(offset);
            };
            let line = self.line_of(offset);
            match b {
                b']' | b'}' => {
                    open.pop();
                    offset += 1;
                    if open.is_empty() {
                        return Ok(offset);
                    }
                    expect_node = false;
                }
                b',' => {
                    if let Some(top) = open.last_mut() {
                        top.1 = false;
                    }
                    offset += 1;
                    expect_node = true;
                }
                b':' if open.last().is_some_and(|top| top.0 && !top.1) => {
                    if let Some(top) = open.last_mut() {
                        top.1 = true;
                    }
                    offset += 1;
                    expect_node = true;
                }
                _ if !expect_node => {
                    // Texto inesperado tras un nodo: serde_yaml lo rechaza.
                    offset += 1;
                }
                b'[' | b'{' => {
                    // Profundidad total: colecciones de bloque abiertas mas las
                    // de flujo.
                    if self.stack.len() + open.len() >= MAX_DEPTH {
                        return Err(error(line, "anidamiento demasiado profundo"));
                    }
                    self.count(line)?;
                    open.push((b == b'{', false));
                    offset += 1;
                    expect_node = true;
                }
                b'&' | b'*' | b'!' => {
                    return Err(error(line, "anclas, alias y etiquetas no soportados"));
                }
                b'\'' | b'"' => {
                    self.count(line)?;
                    offset = quoted_end(text, offset).unwrap_or(text.len());
                    expect_node = false;
                }
                _ => {
                    self.count(line)?;
                    offset = plain_flow_end(text, offset);
                    expect_node = false;
                }
            }
        }
    }

    /// Salta espacios, saltos de linea y comentarios dentro de un flujo.
    fn skip(&self, mut offset: usize) -> usize {
        let text = self.text;
        while let Some(&b) = text.get(offset) {
            match b {
                b' ' | b'\t' | b'\r' | b'\n' => offset += 1,
                b'#' if offset == 0 || matches!(text[offset - 1], b' ' | b'\t' | b'\n') => {
                    while text.get(offset).is_some_and(|&b| b != b'\n') {
                        offset += 1;
                    }
                }
                _ => break,
            }
        }
        offset
    }
}

enum Outcome {
    Done,
    /// Valor en las lineas siguientes: (sangria del dueno, admite secuencia
    /// sin sangria).
    Pending(usize, bool),
    BlockScalar(usize),
    /// Un escalar entre comillas o un flujo termino en este byte.
    Resume(usize),
}

/// Byte siguiente a la comilla de cierre de la cadena que empieza en `start`.
fn quoted_end(text: &[u8], start: usize) -> Option<usize> {
    let quote = *text.get(start)?;
    let mut offset = start + 1;
    while let Some(&b) = text.get(offset) {
        if quote == b'"' && b == b'\\' {
            offset += 2;
            continue;
        }
        if b == quote {
            if quote == b'\'' && text.get(offset + 1) == Some(&b'\'') {
                offset += 2;
                continue;
            }
            return Some(offset + 1);
        }
        offset += 1;
    }
    None
}

/// Fin de un escalar plano dentro de un flujo (puede seguir en otra linea).
fn plain_flow_end(text: &[u8], mut offset: usize) -> usize {
    while let Some(&b) = text.get(offset) {
        if FLOW.contains(&b) {
            return offset;
        }
        if b == b':' {
            let next = text.get(offset + 1).copied();
            if next.is_none_or(|n| matches!(n, b' ' | b'\t' | b'\r' | b'\n') || FLOW.contains(&n)) {
                return offset;
            }
        }
        if b == b'#' && offset > 0 && matches!(text[offset - 1], b' ' | b'\t') {
            return offset;
        }
        offset += 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Result<()> {
        YamlBudget::default().check(text)
    }

    #[test]
    fn accepts_regular_yaml() {
        let text = "\
name: Demo
tags: [a, 'b, c', {x: 1}]
lista:
- uno
- dos: 2
  tres: |
    texto *libre* & con !signos
    - no es secuencia
  cuatro: \"cadena
    de dos lineas: con dos puntos\"
anidado:
  - - a
    - b
plano: Tom & Jerry *no* son alias
  y siguen *en* otra linea
? clave explicita
: valor
";
        check(text).unwrap();
        // Casos que el lector de PuduHub acepta.
        for text in [
            "scope:\n- \n- a\n",
            "scope:\n- a\n  b\n- c\n",
            "scope:\n-\n  a\n-\n- c\n",
            "scope: [multi\n  line, x]\n",
            "scope: [a # c\n  , b]\n",
            "scope: ['q', \"d\"]\n",
            "scope: [a,\n# c\n b]\n",
            "scope: [a\n\n  b]\n",
            "k: {a: [1, 2], b: {c: d}, e}\n",
            "k: {\"a\":1, b :2}\n",
            "k: {k: -, u: http://x/y, v: a:b}\n",
            "k: {a: }\n",
            "k:\n  a:\n  b: [ ]\n  c: { }\n",
            "k:\n  a:\n  - 1\n  - - 2\n    - 3\n  -\n    k: v\n",
            "k:\n  ? \"a\\nb\"\n  : - 3.0\n",
            "k:\n  ? solo\n",
            "k: [{a: 1}]\n",
            "k: [[]]\n",
            "k: ~x\n",
            "layout:\n- kind: toc\n  config:\n",
        ] {
            check(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        }
    }

    #[test]
    fn rejects_anchors_aliases_and_tags() {
        for text in [
            "a: &ancla 1\nb: *ancla\n",
            "a: *ancla\n",
            "a: !!str 1\n",
            "- !Etiqueta x\n",
            "a: [1, *b]\n",
            "a: {k: &x v}\n",
            "&a clave: v\n",
            "a:\n  *alias\n",
        ] {
            let err = check(text).unwrap_err();
            assert!(err.contains("anclas"), "{text:?}: {err}");
        }
    }

    #[test]
    fn rejects_directives_and_multiple_documents() {
        assert!(check("%YAML 1.2\n---\na: 1\n").is_err());
        assert!(check("---\na: 1\n---\nb: 2\n").is_err());
        assert!(check("a: 1\n...\nb: 2\n").is_err());
        check("---\na: 1\n...\n").unwrap();
    }

    #[test]
    fn bounds_block_and_flow_depth() {
        let mut deep = String::new();
        for level in 0..MAX_DEPTH {
            deep.push_str(&" ".repeat(level * 2));
            deep.push_str("k:\n");
        }
        deep.push_str(&" ".repeat(MAX_DEPTH * 2));
        deep.push_str("k: v\n");
        assert!(check(&deep).unwrap_err().contains("profundo"));

        let mut ok = String::new();
        for level in 0..MAX_DEPTH - 1 {
            ok.push_str(&" ".repeat(level * 2));
            ok.push_str("k:\n");
        }
        ok.push_str(&" ".repeat((MAX_DEPTH - 1) * 2));
        ok.push_str("k: v\n");
        check(&ok).unwrap();

        let flow = format!("a: {}{}\n", "[".repeat(40), "]".repeat(40));
        assert!(check(&flow).unwrap_err().contains("profundo"));
        let flow = format!("a: {}{}\n", "[".repeat(31), "]".repeat(31));
        check(&flow).unwrap();
        let flow = format!("a: {}{}\n", "[".repeat(32), "]".repeat(32));
        assert!(check(&flow).unwrap_err().contains("profundo"));

        // Bloque y flujo se suman.
        let mut mixed = String::from("k:\n");
        for level in 1..20 {
            mixed.push_str(&format!("{}k{level}:\n", "  ".repeat(level)));
        }
        mixed.push_str(&format!(
            "{}z: {}{}\n",
            "  ".repeat(20),
            "[".repeat(20),
            "]".repeat(20)
        ));
        assert!(check(&mixed).unwrap_err().contains("profundo"));
    }

    #[test]
    fn bounds_nodes_per_file_and_per_document() {
        let many = "- x\n".repeat(MAX_YAML_NODES as usize + 1);
        assert!(check(&many).unwrap_err().contains("nodos"));
        let flow = format!("a: [{}]\n", "1,".repeat(MAX_YAML_NODES as usize));
        assert!(check(&flow).unwrap_err().contains("nodos"));
        check(&"- x\n".repeat(MAX_YAML_NODES as usize - 1)).unwrap();

        let mut budget = YamlBudget::default();
        let file = "- x\n".repeat(MAX_YAML_NODES as usize);
        for _ in 0..(MAX_DOCUMENT_YAML_NODES / MAX_YAML_NODES) {
            budget.check(&file).unwrap();
        }
        assert!(budget.check("- x\n").is_err());
    }
}
